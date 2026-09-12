//! Agent-runtime contract layer: types / errors / tools / boundary /
//! ports / provider factory / the kernel output vocabulary. Pure types and
//! traits, zero I/O — every implementation crate depends only on this layer.
//!
//! The WebSocket frame vocabulary (`ClientMessage` / `ServerMessage` / …)
//! deliberately lives in the session layer's `protocol` module: frames are
//! BUILT by that layer, so the vocabulary lives with its builder. What
//! stays here is what the kernel itself produces: `WireEvent`, `ErrorCode`,
//! `ChatStateKind`.

mod boundary;
mod error;
mod loop_io;
mod ports;
mod provider;
mod tool;
mod types;
mod wire;

/// Test-only fakes for the contract traits — compiled only under the
/// `test-util` feature (see `test_util.rs` for the rationale).
#[cfg(feature = "test-util")]
pub mod test_util;

pub use error::{CoreError, ErrorCode};
pub use loop_io::{
    Connection, LoopFact, LoopInput, RoundOutcome, StreamEvent, StreamHandle, StreamSink,
};
pub use ports::{OutputPort, ToolPort};
pub use provider::{ModelInfo, Provider};
pub use tool::{BUF_READ_TOOL, QUESTION_TOOL, Tool, ToolCtx, ToolRegistry};
pub use types::{ChatStateKind, Message, Role, StreamChunk, ToolCall, ToolDefinition};
pub use wire::WireEvent;
