//! Data plane of the conversation session: the per-chat task machinery.
//!
//! One spawned conversation = two channel peers over shared primitives:
//! the loop (`flux-loop`) and the round consumer (`round`) — the latter
//! folds the trace AND supervises the tool flights (the `tool_exec`
//! library driven from its select loop). Both are assembled by `spawn`
//! around the chat entity (`chat`: the kernel's `ToolPort` + persistence
//! helpers), state tools (`domain`), the overflow buffer (`buf`), the
//! control handle (`handle`), and the adapter-side tooling (`question`).
//!
//! The control plane — the chat manager (cache/registry), lease/viewer
//! operations, session identity, the event router, task lifecycle, and
//! the proto stream-element vocabulary (`flux-proto`, mapped from the
//! kernel's `WireEvent` by flux-session's router) — lives in
//! `flux-session` (it spans chats and sessions; none of it belongs to a
//! single conversation task). The two planes meet on narrow seams: this
//! crate depends only on flux-core ports (`OutputPort`); `flux-session`
//! depends on this crate, never the reverse.

pub mod buf;
pub mod chat;
pub mod domain;
pub mod handle;
pub mod history;
pub mod question;
pub mod reserved;
pub mod round;
pub mod skills;
pub mod spawn;
pub mod tool_exec;

#[cfg(test)]
mod tests;

pub use chat::ResolvedPin;
pub use domain::INITIAL_STATE;
pub use domain::STATE_TOOL_NAMES;
pub use history::validate_history;
pub use question::QuestionBoard;
