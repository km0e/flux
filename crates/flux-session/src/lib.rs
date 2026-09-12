//! Session layer — the CONTROL PLANE of multi-chat serving.
//!
//! One crate, one concern: everything that outlives (or spans) a single
//! conversation task —
//!
//! - `manager`: the chat manager (`ServerState`) — global config, the chat
//!   cache (hydrated from the store at startup, mutated in place on every
//!   lease/viewer/swap event), and the session identity registry;
//! - `ops`: lease/subscription bookkeeping (claim / open / send / cancel /
//!   rebase / provider switch / question answers) + broadcast + the
//!   detached-session reaper;
//! - `identity`: one client identity (`Session`/`SessionRef`) carrying its
//!   outbound sink — the resume token, the grace window, the routing key;
//! - `router`: the per-chat event fanout (viewers, slow-viewer gap
//!   handling, parked questions) — it maps the kernel's `WireEvent`
//!   vocabulary onto the proto stream elements (`flux-proto`), the ONE
//!   wire contract;
//! - `lifecycle`: lazy task spawn and stale-task replacement.
//!
//! The DATA plane — the per-chat task machinery (loop assembly, round
//! consumer + its supervised tool flights, chat entity, state tools,
//! buffers) — lives in `flux-chat`. The seams between the planes are
//! narrow and one-way: this crate depends on flux-chat (spawn,
//! `ChatHandle`, `QuestionBoard`, `ResolvedPin`), while flux-chat depends
//! only on flux-core ports (`OutputPort`).

pub mod identity;
pub mod lifecycle;
pub mod manager;
pub mod ops;
pub mod router;

#[cfg(test)]
pub(crate) mod test_util;
#[cfg(test)]
mod tests;

pub use identity::{Session, SessionRef, SessionSink};
pub use manager::{ChatInfoOwned, ServerState};
pub use ops::spawn_session_reaper;
pub use ops::{
    CancelOutcome, ClaimOutcome, MutateOutcome, OpenOutcome, QuestionOutcome, SendOutcome,
    SwitchOutcome,
};
