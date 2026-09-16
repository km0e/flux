//! spawn — assemble the loop and its channel peers from adapter parts.
//!
//! Two peers share one input channel and one fact trace:
//! - the **loop** (flux-loop): pure machine pump;
//! - the **round consumer** (this crate): folds the trace — persistence,
//!   routing, provider triggering, supervised tool flights (the tool_exec
//!   library driven from the consumer's select loop), connection
//!   replacement.
//!
//! The provider instance arrives already resolved (the server registry
//! selected it at creation/swap/hydration); this layer calls `begin` —
//! the chat-owned materials (system prompt, tool definitions, history)
//! meet the connection here.

use crate::chat::{Chat, ChatInit};
use crate::domain::{StateGetTool, StateManager, StateSetTool};
use crate::handle::ChatHandle;
use crate::question::QuestionTool;
use crate::round::{RoundControl, RoundDeps, run_round};
use crate::skills::{
    SkillActivationIndex, SkillReadTool, compose_system_prompt, derive_activation_index,
};
use flux_core::{OutputPort, ToolRegistry};
use flux_store::Store;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;

/// Per-chat resources the registry assembly binds tools to.
pub(crate) struct ChatKit<'a> {
    /// Initial state key → description (drives the state tools' schema).
    pub(crate) descriptions: &'a HashMap<String, String>,
    /// The chat's `question` tool (board + output sink pre-wired).
    pub(crate) question: QuestionTool,
}

/// Build the per-chat registry: global entries plus the chat-owned tools
/// bound to this chat (question, buf_read, skill_read, state tools).
/// Chat-owned tools use `register_if_absent` — FIRST registration wins,
/// so a global entry with the same name would shadow the chat-owned one;
/// that cannot happen through MCP (assembly-time reserved-name check in
/// `reserved.rs`), which is what keeps the chat-owned tools
/// authoritative for their names.
pub(crate) fn assemble_tools(
    global: &ToolRegistry,
    state_manager: Arc<StateManager>,
    kit: &ChatKit<'_>,
    chat_id: &str,
    store: &Arc<Store>,
    skill_index: Arc<SkillActivationIndex>,
) -> ToolRegistry {
    let registry = ToolRegistry::new();
    for tool in global.entries() {
        registry.register(tool);
    }
    // question — the model asks the user a question mid-round (the
    // approval prompt's ecological successor; content agent-produced).
    registry.register_if_absent(Arc::new(kit.question.clone()));
    registry.register_if_absent(Arc::new(StateGetTool::new(
        state_manager.clone(),
        kit.descriptions,
    )));
    registry.register_if_absent(Arc::new(StateSetTool::new(state_manager, kit.descriptions)));
    // buf_read — the per-chat overflow reader. Store-backed (see buf.rs):
    // entries are anchored to their tool calls and survive rebuilds and
    // restarts without any shell handoff.
    registry.register_if_absent(Arc::new(crate::buf::BufReadTool::new(
        Arc::clone(store),
        chat_id.to_string(),
    )));
    // skill_read — the chat-owned activation tool (see skills.rs): the
    // derived activation index turns a re-read of unchanged skill content
    // into a short note instead of a duplicate injection.
    registry.register_if_absent(Arc::new(SkillReadTool::new(skill_index)));
    registry
}

/// Spawn a conversation: the loop + its two channel peers. Returns the
/// control handle — the consumer task lives for the chat's lifetime
/// (rebuilds happen in place; only a crash or chat deletion ends it).
/// Infallible: assembly is pure wiring (store reads inside log and
/// degrade, `begin` is infallible).
/// Called by the session layer's task lifecycle (and tests).
pub async fn spawn(
    init: ChatInit,
    system_prompt: Arc<str>,
    tools: Arc<ToolRegistry>,
    store: Arc<Store>,
    initial_state: Arc<HashMap<String, String>>,
    // `wire`: the sink the consumer forwards facts to (production: the
    // router; tests: a collecting mock).
    wire: Arc<dyn OutputPort>,
) -> ChatHandle {
    let state_manager =
        Arc::new(StateManager::for_chat(store.clone(), &init.id, &initial_state).await);
    // The Tier-1 catalog rides the preamble as a begin-time snapshot; the
    // base stays unpolluted in RoundDeps so every rebuild recomposes
    // fresh (skills.rs).
    let composed_prompt = compose_system_prompt(&system_prompt, state_manager.workdir());
    // The activation index derives from the handed-over history — a fork
    // or respawn therefore inherits exactly the activations its copied
    // transcript carries (skills.rs).
    let skill_index = derive_activation_index(&store, &init.id, &init.history).await;
    // The question tool emits straight to the router (adapter-side tooling;
    // the loop's fact trace carries everything else).
    let question = QuestionTool::new(Arc::clone(&init.questions), Arc::clone(&wire));
    let kit = ChatKit {
        descriptions: &initial_state,
        question,
    };
    let tool_registry = Arc::new(assemble_tools(
        &tools,
        state_manager.clone(),
        &kit,
        &init.id,
        &store,
        skill_index,
    ));

    let tool_defs: Arc<[flux_core::ToolDefinition]> =
        Arc::from(tool_registry.definitions().into_boxed_slice());

    // The chat's connection: `begin` over the chat-owned materials. The
    // instance was resolved upstream (server registry); this is the
    // assembly point, not a selection point.
    let connection = init
        .provider
        .begin(&composed_prompt, &tool_defs, &init.history);

    let chat = Arc::new(Chat {
        id: init.id.clone(),
        state_manager,
        tools: tool_registry.clone(),
        store: store.clone(),
    });

    // Channels: one input FIFO (every peer), one fact trace (bounded — the
    // loop's backpressure surface), one control channel (ops → consumer).
    // The two unbounded channels are deliberate: senders never block or
    // drop — user turns enqueue regardless of round boundaries (also under
    // an armed rebuild gate) and provider stream chunks must never be
    // lost; their natural bound is one round's chunks plus queued turns.
    let (loop_tx, loop_rx) = mpsc::unbounded_channel::<flux_core::LoopInput>();
    let (facts_tx, facts_rx) = mpsc::channel::<flux_core::LoopFact>(flux_loop::OUT_CAPACITY);
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel::<crate::round::RoundCmd>();

    // Peer 1: the loop (pure machine pump).
    let loop_task =
        tokio::spawn(flux_loop::Loop::new(flux_loop::Machine::new(), loop_rx, facts_tx).run());

    // Peer 2: the round consumer (the fact-trace fold; owns the done flag
    // and the supervised tool flights).
    let done = Arc::new(AtomicBool::new(false));
    let state_slot = Arc::new(StdMutex::new(flux_core::ChatStateKind::Idle));
    let deps = RoundDeps {
        chat: chat.clone(),
        wire: Arc::clone(&wire),
        state_slot: state_slot.clone(),
        connection,
        // In-place rebuild materials: the consumer re-assembles from
        // these at every fired gate (the global registry's Arc sees MCP
        // mutations; the provider instance is replaced by a carried pin).
        system_prompt,
        global_registry: Arc::clone(&tools),
        descriptions: Arc::clone(&initial_state),
        questions: Arc::clone(&init.questions),
        provider: Arc::clone(&init.provider),
    };
    let consumer_task = tokio::spawn(run_round(
        facts_rx,
        ctrl_rx,
        loop_tx.clone(),
        deps,
        done.clone(),
    ));

    let aborts: Vec<tokio::task::AbortHandle> =
        vec![loop_task.abort_handle(), consumer_task.abort_handle()];
    ChatHandle::new(
        loop_tx,
        RoundControl::new(ctrl_tx),
        aborts,
        done,
        state_slot,
    )
}
