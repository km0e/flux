//! Tool execution port — the one remaining port the kernel knows.
//!
//! The other legacy ports are gone: the provider is a [`Connection`]
//! (channel peer, `loop_io`), persistence is a fold over the loop's fact
//! trace, and output IS the fact trace. The tool executor consumes
//! `ToolDispatched` facts and pushes `ToolFinished` inputs — but it calls
//! THIS port to actually run tools.

use crate::ToolCall;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

/// Tool execution.
///
/// The adapter owns the whole pipeline per call: existence check → ctx
/// enrichment (the chat boundary — `workdir` / `current_dir` — fills the
/// [`ToolCtx`] so tools resolve paths against it) → execution → output
/// bounding. Errors normalize into the result string — the model sees
/// every failure and can self-correct. Flux is a no-approval agent: there is
/// no confirmation step.
#[async_trait]
pub trait ToolPort {
    /// Execute a tool (state tools or registry tools) under the
    /// invocation context.
    async fn execute(
        &self,
        call: &ToolCall,
        args: HashMap<String, Value>,
        ctx: crate::ToolCtx,
    ) -> String;
}

// Blanket delegate so shared adapters (Arc<Chat>, test doubles behind an
// Arc) satisfy the port directly.
#[async_trait]
impl<T: ToolPort + Send + Sync + ?Sized> ToolPort for std::sync::Arc<T> {
    async fn execute(
        &self,
        call: &ToolCall,
        args: HashMap<String, Value>,
        ctx: crate::ToolCtx,
    ) -> String {
        (**self).execute(call, args, ctx).await
    }
}

/// Client output — a typed wire-event sink for adapter-side emitters
/// (the `question` tool). The loop itself does not use this: its output
/// is the fact trace; this port is how adapter tooling reaches the
/// router without knowing the transport.
#[async_trait]
pub trait OutputPort: Send + Sync {
    /// Emit one wire event for this chat (the adapter owns the chat id).
    async fn emit(&self, event: crate::WireEvent);
}

#[async_trait]
impl<T: OutputPort + Send + Sync + ?Sized> OutputPort for std::sync::Arc<T> {
    async fn emit(&self, event: crate::WireEvent) {
        (**self).emit(event).await
    }
}
