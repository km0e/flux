//! Generated protobuf/tonic contracts for the flux wire protocol.
//!
//! `proto/` (repo root) is the single source of truth (P4 spike, form 3a);
//! tonic-build compiles it into this crate at build time. Downstream crates
//! depend on `flux-proto` instead of hand-mirroring frames — the three-place
//! manual sync (protocol.rs / types.ts / docs) collapses to one `.proto` +
//! codegen as the migration proceeds family by family.
//!
//! The generated module mirrors the proto package (`flux.v1`).

pub mod flux {
    // tonic's generated service traits carry large `Result::Err` variants —
    // clippy's `result_large_err` would otherwise fail `-D warnings` on
    // codegen nobody authors by hand.
    #[allow(clippy::result_large_err)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/flux.v1.rs"));
    }
}

/// Re-exported so downstream crates (tests, tooling) can decode/encode the
/// generated messages without adding their own prost dependency.
pub use prost;

/// Contract→wire type conversions (flux-core vocabulary ↔ proto wire
/// types). This is the ONE mapping between the two vocabularies — the
/// old hand-mirrored JSON protocol collapsed into generated types plus
/// these From impls, all compile-exhaustive over the source enums.
pub mod conv {
    use super::flux::v1;
    use flux_core::ErrorCode;

    impl From<flux_core::Role> for v1::Role {
        fn from(r: flux_core::Role) -> Self {
            match r {
                flux_core::Role::System => v1::Role::System,
                flux_core::Role::User => v1::Role::User,
                flux_core::Role::Assistant => v1::Role::Assistant,
                flux_core::Role::Tool => v1::Role::Tool,
            }
        }
    }

    impl From<flux_core::ToolCall> for v1::ToolCall {
        fn from(t: flux_core::ToolCall) -> Self {
            v1::ToolCall {
                id: t.id,
                name: t.name,
                arguments: t.arguments,
            }
        }
    }

    impl From<flux_core::Message> for v1::Message {
        fn from(m: flux_core::Message) -> Self {
            v1::Message {
                role: v1::Role::from(m.role) as i32,
                content: m.content,
                reasoning_content: m.reasoning_content,
                tool_calls: m.tool_calls.into_iter().map(Into::into).collect(),
                tool_call_id: m.tool_call_id,
                // Persistence row id — the core Message carries none; the
                // history-snapshot path (StoredMessage) stamps it after
                // this conversion.
                id: 0,
            }
        }
    }

    impl From<flux_core::ChatStateKind> for v1::ChatStateKind {
        fn from(s: flux_core::ChatStateKind) -> Self {
            match s {
                flux_core::ChatStateKind::Idle => v1::ChatStateKind::Idle,
                flux_core::ChatStateKind::Streaming => v1::ChatStateKind::Streaming,
            }
        }
    }

    impl From<ErrorCode> for v1::ErrorCode {
        fn from(c: ErrorCode) -> Self {
            match c {
                ErrorCode::ProviderConnection => v1::ErrorCode::ProviderConnection,
                ErrorCode::ToolExecution => v1::ErrorCode::ToolExecution,
                ErrorCode::InvalidArguments => v1::ErrorCode::InvalidArguments,
                ErrorCode::ChatBusy => v1::ErrorCode::ChatBusy,
                ErrorCode::ChatNotFound => v1::ErrorCode::ChatNotFound,
                ErrorCode::StreamCrashed => v1::ErrorCode::StreamCrashed,
                ErrorCode::StreamGap => v1::ErrorCode::StreamGap,
                ErrorCode::InvalidRequest => v1::ErrorCode::InvalidRequest,
                ErrorCode::Internal => v1::ErrorCode::Internal,
            }
        }
    }
}
