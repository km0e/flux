//! # flux-loop — the conversation loop as a pure state machine.
//!
//! The kernel of the agent: a pure reducer ([`machine::Machine`]) between
//! two channels. It consumes [`LoopInput`]s (the shared input vocabulary,
//! pushed by the session layer, the provider connection, and the tool
//! executor) and emits [`LoopFact`]s — the semantic fact trace the chat
//! layer folds. The loop holds no I/O, no trait objects, no provider, no
//! persistence: every collaborator is a channel peer wired by the
//! adapter. Conversation kinds (`classic` / `feature`) differ only in
//! registered tools + adapter-side wiring — the machine is kind-agnostic.
//!
//! Design rationale: the kernel is a pure state machine.

pub mod machine;
pub mod runtime;

pub use machine::{Machine, State, Step, StreamOutput, ToolCallWithArgs};
pub use runtime::{Loop, OUT_CAPACITY};
