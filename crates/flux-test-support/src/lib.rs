//! Shared test-support crate — a DEV-DEPENDENCY ONLY crate.
//!
//! Every crate whose tests touch loopback HTTP (or whose assertions depend
//! on the host environment at all) carries the same two hazards:
//!
//! 1. **Proxy env vars hijack loopback requests.** A developer shell's
//!    `http_proxy`/`https_proxy`/`all_proxy` silently detours reqwest's
//!    in-process clients — every 127.0.0.1 request goes through the proxy
//!    and comes back 502, surfacing as phantom test failures (this exact
//!    failure mode produced 17 phantom grpc test failures before the
//!    guard existed; see AGENTS.md).
//! 2. **`FLUX_*` vars re-point the very flags tests assert on** (the e2e
//!    child processes inherit them too).
//!
//! This crate is the single home for the fix: the env-var list lives ONCE
//! (previously it was copy-pasted per test binary), the
//! [`test_env_guard!`](macro@crate::test_env_guard) macro installs it as a
//! pre-main ctor, and [`loopback_client`] builds a proxy-proof client for
//! tests that drive raw loopback servers.
//!
//! It must only ever appear under `[dev-dependencies]`: the guard mutates
//! process-global state and is only legal pre-`main` in a test process.
//! The macro emits its module under `#[cfg(test)]`, so even an accidental
//! regular dependency cannot leak the strip into a production build.

// Re-exported so the `test_env_guard!` expansion can name the `#[ctor]`
// attribute without consumers needing a direct `ctor` dependency.
pub use ctor;

/// Strip the host-shell variables the tests must be hermetic against:
/// proxy vars (reqwest's `system-proxy` would detour loopback requests
/// through the proxy) and `FLUX_*` vars (they re-point the flags tests
/// assert on).
///
/// Must run before the test harness spawns ANY test thread — always
/// install it through [`test_env_guard!`](macro@crate::test_env_guard),
/// never call this from a running test (racing env mutation).
pub fn strip_host_env() {
    // SAFETY: `env::remove_var` is a process-global mutation (marked
    // unsafe in edition 2024). Legal here only because the ctor runs
    // pre-`main` — before the test harness starts any thread — so no
    // reader can race it; tests setting their own vars afterwards are
    // unaffected.
    unsafe {
        for var in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
        ] {
            std::env::remove_var(var);
        }
        let flux_vars: Vec<String> = std::env::vars()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with("FLUX_"))
            .collect();
        for key in flux_vars {
            std::env::remove_var(&key);
        }
    }
}

/// A reqwest client builder for driving raw loopback test servers: proxy
/// detection disabled, so a host shell's proxy vars can never detour
/// 127.0.0.1 requests (the phantom-502 failure mode). Belt-and-suspenders
/// alongside [`test_env_guard!`](macro@crate::test_env_guard): the guard
/// covers the process, and `.no_proxy()` covers a client built after some
/// test set a proxy var of its own. Chain extra knobs (timeouts, …) on the
/// builder before `.build()`.
pub fn loopback_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}

/// Convenience form of [`loopback_client_builder`] — a built client with
/// default knobs and proxy detection disabled.
pub fn loopback_client() -> reqwest::Client {
    loopback_client_builder()
        .build()
        .expect("loopback test client build")
}

/// Install the hermeticity guard for the current test binary: expands to a
/// `#[cfg(test)]` module whose ctor strips the hostile env vars before any
/// test thread starts.
///
/// One line per test target, at the target's root (`lib.rs` for unittests,
/// `tests/*.rs` for integration tests — they are SEPARATE binaries and the
/// lib's guard does not cover them):
///
/// ```ignore
/// flux_test_support::test_env_guard!();
/// ```
#[macro_export]
macro_rules! test_env_guard {
    () => {
        #[cfg(test)]
        mod test_env_guard {
            // The attribute resolves through this crate's re-export of
            // `ctor`, and `crate_path` redirects ctor's own generated
            // support code to the re-exported crate — consumers need no
            // direct `ctor` dependency.
            #[::flux_test_support::ctor::ctor(crate_path = ::flux_test_support::ctor)]
            fn strip_host_env() {
                $crate::strip_host_env();
            }
        }
    };
}
