pub mod openai;
pub mod sse;

pub use openai::{OpenAiConfig, OpenAiParams, OpenAiProvider};

// Hermeticity guard: proxy/`FLUX_*` env vars must never leak into the test
// process (the loopback mock servers are raw TCP — a host proxy turns every
// reqwest call into a phantom 502). Shared across all test binaries via
// flux-test-support; tests/models.rs is a SEPARATE binary and carries its
// own invocation. cfg(test): the crate is a dev-dependency, invisible to
// the non-test build.
#[cfg(test)]
flux_test_support::test_env_guard!();
