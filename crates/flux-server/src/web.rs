//! Browser UI static site — served by the SAME axum server as the
//! Connect surface (`/flux.v1.*`) and the terminal side channel
//! (`/ws/term`) — one process, one port. The page is a viewer/controller
//! that connects back over the same origin, so the static layer stays a
//! pure leaf — it never proxies anything.
//!
//! Serving model — EMBEDDED BASE + DISK OVERRIDE (Gitea `custom/` style):
//! the build embeds `clients/web/dist` into the binary (feature
//! `web-ui-embed`; release embeds, debug reads the repo dist from disk —
//! the dev fallback, pinned to CARGO_MANIFEST_DIR so the process CWD is
//! irrelevant). On top of that base, ONE optional override directory
//! (`--web-assets-dir` or `~/.flux/web-ui`) shadows files PER PATH: a file
//! present on disk wins, everything else falls through to the embedded
//! bundle. There is no resolution CHAIN anymore and no unpacked bundle to
//! go stale — the structurally-mismatched state (binary newer than its
//! assets) cannot exist for the embedded half. A partial override can mix
//! versions (user index.html + embedded assets of another build) — that is
//! the accepted price of user customization, flagged at startup when the
//! override carries a stale version stamp (see main.rs).
//!
//! Header policy split:
//! - Cache-Control is PER-PATH policy (by build convention, not by source):
//!   hashed `assets/*` get `immutable`, root-level unhashed files
//!   (index.html, manifest.webmanifest) get `no-store` — they reference the
//!   hashed names, and a cached index would point at old hashes after a
//!   rebuild (a white page). Embedded files additionally carry a strong
//!   ETag (the compile-time sha256) so conditional requests 304.
//! - Security headers are UNIFORM policy: one CSP header + nosniff via two
//!   `SetResponseHeaderLayer`s composed by `ServiceBuilder`, applied over
//!   every route AND the fallback. `img-src 'self'` is load-bearing: the
//!   page runs on plain http, so same-origin images (the favicon) need it —
//!   `https:` alone does not match them.
//! - The body-mechanics (Content-Length vs chunked, Vary, conditional
//!   requests) stay inside the handler: CompressionLayer owns transfer
//!   headers and skips 304s/empty bodies; the header layers only touch
//!   their own headers.
//!
//! Security posture: the server acts with the starting user's
//! permissions; the port is bound to 127.0.0.1 by default. Remote exposure
//! = reverse proxy with TLS, an explicit user decision. The override
//! layer is the user's own directory; path resolution rejects any
//! non-normal component (`..`, absolute, prefix) — the traversal guard
//! ServeDir used to provide, kept for the disk half.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::path::{Component, Path as StdPath, PathBuf};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;

/// CSP for the served page — strict posture: nothing but the app's own
/// hashed assets and its WebSocket.
///
/// `script-src` carries the sha256 of the ONE inline script the build
/// emits: index.html's theme pre-paint block (applies the persisted
/// dark/light choice before first paint — a hash instead of
/// 'unsafe-inline' keeps the XSS bar high). If that script ever changes,
/// recompute: `openssl dgst -sha256 -binary | openssl base64` over the
/// exact bytes between `<script>` and `</script>` (the `csp_pairing` test
/// below cross-checks it against the built dist when present).
///
/// `font-src 'self'` — the bundled terminal fonts (styles/fonts.css).
/// Without it the faces fall under default-src 'none' and every load
/// errors (found via the terminal: prompts rendered with fallback
/// metrics). `connect-src 'self' ws: wss:` — the app talks same-origin
/// fetches (the Connect calls) plus one WebSocket (`/ws/term`, which may
/// legitimately be `wss:` behind a reverse proxy); `'self'` also covers
/// e.g. Chrome DevTools' `/.well-known/appspecific` probe, which would
/// otherwise surface a CSP error in the console on every DevTools open.
pub(crate) const CSP: &str = "default-src 'none'; script-src 'self' 'sha256-yC4B8JEmv+Lsb18mJYR1IQxRZva/4bRpPXJ59/85y9g='; style-src 'self' 'unsafe-inline'; \
                   img-src 'self' https: data:; font-src 'self'; manifest-src 'self'; connect-src 'self' ws: wss:; base-uri 'none'; frame-ancestors 'none'";

/// Cache-Control for hashed assets — the body at any given name never
/// changes, so cache it for a year and never re-validate.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";

/// The embedded UI bundle. Release builds bake `clients/web/dist` into
/// the binary; debug builds (no `debug-embed`) resolve at RUNTIME against
/// the compile-time repo path — edit + rebuild the UI, refresh the page,
/// no `cargo build` in between (the dev disk fallback).
#[cfg(feature = "web-ui-embed")]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../clients/web/dist"]
struct WebAssets;

/// The static site's configuration: the embedded base is unconditional
/// (feature `web-ui-embed`), `override_dir` is the optional user
/// customization layer that shadows it per path.
#[derive(Clone)]
pub(crate) struct WebUi {
    pub(crate) override_dir: Option<PathBuf>,
}

/// The static-site routes (`/`, `/index.html`, `/manifest.webmanifest`,
/// `/assets/*`, 404 fallback), ready to merge into the transport router.
/// No startup validation: an incomplete override or a placeholder embed
/// surfaces at request time — index 500/404 + a warn log — a deliberate
/// tradeoff (see decisions.md T-14): the server must not refuse to boot
/// over UI assets.
///
/// Why explicit path families instead of one catch-all: the cache-policy
/// boundary is irreducible (immutable `assets/*` vs `no-store` root files)
/// and the 404 contract must stay tight — unknown top-level paths never
/// serve a file just because an override happens to contain one.
pub(crate) fn router(ui: &WebUi) -> Router {
    // The asset route: compression (on-the-fly gzip) + the resolver. Hashed
    // assets are immutable-cached, so compression cost lands on cold loads
    // only; the layer is applied to THIS route only (root files are tiny).
    let assets = get(serve_asset).layer(CompressionLayer::new());

    let security = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ));

    Router::new()
        .route("/", get(serve_index))
        .route("/index.html", get(serve_index))
        .route("/manifest.webmanifest", get(serve_manifest))
        .route("/assets/{*path}", assets)
        .fallback(not_found)
        .layer(security)
        .with_state(ui.clone())
}

/// Uniform 404 for everything that is not `/flux.v1.*`, `/ws/term`, or a
/// static file. Security headers ride the router's layer — see [`router`].
async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "not found\n",
    )
        .into_response()
}

/// Root files: `no-store` so a stale HTML/manifest never outlives its
/// build (it references the hashed asset names).
async fn serve_index(State(ui): State<WebUi>, headers: HeaderMap) -> Response {
    serve_rel(&ui, "index.html", &headers).await
}

async fn serve_manifest(State(ui): State<WebUi>, headers: HeaderMap) -> Response {
    serve_rel(&ui, "manifest.webmanifest", &headers).await
}

/// The `/assets/*` family — hashed, immutable, gzip on the wire. The
/// wildcard captures relative to `/assets/`; the dist-root-relative name
/// re-carries the `assets/` prefix (the old nest_service did the
/// equivalent splice into ServeDir's root).
async fn serve_asset(
    State(ui): State<WebUi>,
    Path(rel): Path<String>,
    headers: HeaderMap,
) -> Response {
    // Normalize the wildcard capture (it includes the leading slash)
    // before the path safety pass — the traversal guard is `sanitize`.
    let captured = rel.trim_start_matches('/');
    serve_rel(&ui, &format!("assets/{captured}"), &headers).await
}

/// The two-layer resolver: override disk → embedded bundle → 404.
async fn serve_rel(
    ui: &WebUi,
    rel: &str,
    #[cfg_attr(not(feature = "web-ui-embed"), allow(unused_variables))] req_headers: &HeaderMap,
) -> Response {
    let Some(rel) = sanitize(rel) else {
        return not_found().await;
    };

    // 1. User override: a file on disk shadows the embedded base for this
    //    path and this path only.
    if let Some(root) = &ui.override_dir {
        let path = root.join(&rel);
        if path.is_file() {
            return match tokio::fs::read(&path).await {
                Ok(body) => respond(&rel, body.into(), None),
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "override file unreadable at request time");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                        "override file unavailable\n",
                    )
                        .into_response()
                }
            };
        }
    }

    // 2. The embedded base.
    #[cfg(feature = "web-ui-embed")]
    if let Some(file) = WebAssets::get(&rel) {
        // Compile-time sha256 → strong ETag → conditional requests 304.
        let etag = hex_tag(&file.metadata.sha256_hash());
        if req_headers
            .get(header::IF_NONE_MATCH)
            .is_some_and(|v| etag_matches(v, &etag))
        {
            return (
                StatusCode::NOT_MODIFIED,
                [
                    (
                        header::CACHE_CONTROL,
                        HeaderValue::from_static(cache_control(&rel)),
                    ),
                    (
                        header::ETAG,
                        HeaderValue::from_str(&etag).expect("etag is ASCII"),
                    ),
                ],
                axum::body::Bytes::new(),
            )
                .into_response();
        }
        let body = match file.data {
            std::borrow::Cow::Borrowed(b) => axum::body::Bytes::from_static(b),
            std::borrow::Cow::Owned(o) => axum::body::Bytes::from(o),
        };
        return respond(&rel, body, Some(etag));
    }

    tracing::trace!(path = %rel, "no UI asset for path");
    not_found().await
}

/// Assemble a 200 response: path-derived MIME + cache policy, optional
/// ETag, body as-is (the compression layer, applied on the assets route,
/// owns the transfer encoding).
fn respond(rel: &str, body: axum::body::Bytes, etag: Option<String>) -> Response {
    let mut res = (StatusCode::OK, axum::body::Body::from(body)).into_response();
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(rel)),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control(rel)),
    );
    if let Some(etag) = etag
        && let Ok(v) = HeaderValue::from_str(&etag)
    {
        headers.insert(header::ETAG, v);
    }
    res
}

/// Cache policy by BUILD CONVENTION, not by source: hashed assets are
/// immutable wherever they come from; the unhashed root files must
/// revalidate. Unknown paths default to no-store (never pin something
/// that might change under the same name).
fn cache_control(rel: &str) -> &'static str {
    if rel.starts_with("assets/") {
        ASSET_CACHE
    } else {
        "no-store"
    }
}

/// MIME by extension — the handful of types the build emits, nothing
/// speculative. `text/javascript` is the modern, CSP-compatible spelling
/// for scripts.
fn content_type(rel: &str) -> &'static str {
    match StdPath::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "webmanifest" => "application/manifest+json",
        "json" => "application/json",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Restrict the override/embedded lookup to plain relative paths: every
/// component must be Normal. Rejects `..`, `.` (no-op segments), absolute
/// paths and Windows prefixes; backslash tricks die here on Windows
/// (where `\` separates) and are literal filenames on unix (harmless).
/// This is the traversal guard for the disk half — the `..%2F` unit test
/// below pins it.
fn sanitize(rel: &str) -> Option<String> {
    if rel.is_empty() {
        return None;
    }
    // NUL never resolves on any platform's filesystem and must not reach
    // the embedded map either.
    if rel.contains('\0') {
        return None;
    }
    let path = StdPath::new(rel);
    let mut clean = String::with_capacity(rel.len());
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => {
                clean.push('/');
                clean.push_str(seg.to_str()?);
            }
            _ => return None,
        }
    }
    // At least one component was accepted, so `clean` carries exactly one
    // leading separator — strip it (the caller joins against a root).
    clean.remove(0);
    Some(clean)
}

/// Hex-encode the compile-time sha256 into a quoted strong ETag.
#[cfg_attr(not(feature = "web-ui-embed"), allow(dead_code))]
fn hex_tag(hash: &[u8; 32]) -> String {
    let mut out = String::with_capacity(2 + hash.len() * 2);
    out.push('"');
    for b in hash {
        out.push_str(&format!("{b:02x}"));
    }
    out.push('"');
    out
}

/// `If-None-Match` matching: `*` or a comma list of tags, weak prefixes
/// (`W/`) tolerated by stripping them.
#[cfg_attr(not(feature = "web-ui-embed"), allow(dead_code))]
fn etag_matches(if_none_match: &HeaderValue, etag: &str) -> bool {
    if_none_match
        .to_str()
        .map(|raw| {
            raw.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*" || candidate.trim_start_matches("W/").trim() == etag
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// A minimal override site in a temp dir. The embedded base (repo dist
    /// in debug builds) stays underneath — these tests pin that the
    /// override shadows it and unknown paths still 404.
    async fn site() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html>flux</html>").unwrap();
        std::fs::write(
            dir.path().join("manifest.webmanifest"),
            "{\"name\":\"flux\"}",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(
            dir.path().join("assets/index-AbCd1234.js"),
            "//js body long enough to satisfy the 32-byte compression floor",
        )
        .unwrap();
        std::fs::write(dir.path().join("assets/index-EfGh5678.css"), "/*css*/").unwrap();
        let ui = WebUi {
            override_dir: Some(dir.path().to_path_buf()),
        };
        (router(&ui), dir)
    }

    async fn send(app: Router, req: Request<Body>) -> axum::response::Response {
        app.oneshot(req).await.unwrap()
    }

    fn get_req(target: &str) -> Request<Body> {
        Request::builder().uri(target).body(Body::empty()).unwrap()
    }

    async fn body_text(res: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    #[tokio::test]
    async fn serves_index_with_security_headers_and_no_store() {
        let (app, _dir) = site().await;
        let res = send(app, get_req("/")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(body_text(res).await, "<html>flux</html>");
        let (app, _dir) = site().await;
        let res = send(app, get_req("/index.html")).await;
        assert_eq!(res.status(), 200);
        let headers = res.headers();
        assert_eq!(headers.get("cache-control").unwrap(), "no-store");
        let csp = headers
            .get("content-security-policy")
            .unwrap()
            .to_str()
            .unwrap();
        // 'self' in img-src is load-bearing: the page runs on plain http, so
        // without it the same-origin favicon is blocked by the CSP.
        assert!(csp.contains("img-src 'self'"), "csp: {csp}");
        // default-src is 'none', so the manifest needs its own directive —
        // without manifest-src the PWA webmanifest fetch falls back to
        // default-src and is blocked.
        assert!(csp.contains("manifest-src 'self'"), "csp: {csp}");
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
    }

    #[tokio::test]
    async fn serves_assets_immutable_and_404s_unknown_paths() {
        let (app, _dir) = site().await;
        let res = send(app, get_req("/assets/index-AbCd1234.js")).await;
        assert_eq!(res.status(), 200);
        let headers = res.headers().clone();
        assert_eq!(
            headers.get("cache-control").unwrap(),
            "public, max-age=31536000, immutable"
        );
        assert!(body_text(res).await.starts_with("//js body"));
        let (app, _dir) = site().await;
        let res = send(app, get_req("/assets/nope.js")).await;
        assert_eq!(res.status(), 404);
        // A path that never appeared in the build 404s too — sanitize()
        // drops traversal segments before anything touches the disk or
        // the embedded map.
        let (app, _dir) = site().await;
        let res = send(app, get_req("/assets/..%2Findex.html")).await;
        assert_ne!(res.status(), 200);
    }

    #[tokio::test]
    async fn serves_manifest_no_store() {
        let (app, _dir) = site().await;
        let res = send(app, get_req("/manifest.webmanifest")).await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "application/manifest+json"
        );
        assert_eq!(res.headers().get("cache-control").unwrap(), "no-store");
        assert!(body_text(res).await.contains("flux"));
    }

    #[tokio::test]
    async fn gzip_is_negotiated_and_decodes_to_the_raw_asset() {
        let (app, _dir) = site().await;
        let req = Request::builder()
            .uri("/assets/index-AbCd1234.js")
            .header("accept-encoding", "gzip, deflate, br")
            .body(Body::empty())
            .unwrap();
        let res = send(app, req).await;
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers().get("content-encoding").unwrap(), "gzip");
        let compressed = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut raw).unwrap();
        assert!(String::from_utf8_lossy(&raw).starts_with("//js body"));

        // Without the header the raw body ships verbatim.
        let (app, _dir) = site().await;
        let res = send(app, get_req("/assets/index-AbCd1234.js")).await;
        assert!(res.headers().get("content-encoding").is_none());
        assert!(body_text(res).await.starts_with("//js body"));
    }

    #[tokio::test]
    async fn head_requests_get_headers_without_a_body() {
        let (app, _dir) = site().await;
        let req = Request::builder()
            .method("HEAD")
            .uri("/assets/index-AbCd1234.js")
            .body(Body::empty())
            .unwrap();
        let res = send(app, req).await;
        assert_eq!(res.status(), 200);
        assert!(res.headers().get("content-length").is_some());
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(bytes.is_empty(), "HEAD must not ship a body");
    }

    #[tokio::test]
    async fn non_get_methods_are_rejected() {
        let (app, _dir) = site().await;
        for target in ["/", "/assets/index-AbCd1234.js"] {
            let req = Request::builder()
                .method("POST")
                .uri(target)
                .body(Body::empty())
                .unwrap();
            let res = send(app.clone(), req).await;
            assert_eq!(res.status(), 405, "POST {target} must be a 405");
        }
    }

    #[tokio::test]
    async fn fallback_404_carries_security_headers() {
        let (app, _dir) = site().await;
        let res = send(app, get_req("/nowhere")).await;
        assert_eq!(res.status(), 404);
        assert!(res.headers().get("content-security-policy").is_some());
        assert_eq!(
            res.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
    }
}

/// Tests for the EMBEDDED half. In debug builds rust-embed is dynamic —
/// `WebAssets` reads the repo dist at runtime — so these exercise the same
/// resolver path the release binary uses, against whatever dist exists
/// (the real build, or build.rs's placeholder on toolchain-less
/// checkouts). Assertions are therefore driven by `WebAssets::iter()`,
/// never by hardcoded hashed names.
#[cfg(all(test, feature = "web-ui-embed"))]
mod embedded_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn bare_router() -> Router {
        router(&WebUi { override_dir: None })
    }

    async fn send(app: Router, req: Request<Body>) -> axum::response::Response {
        app.oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn embedded_index_serves_with_no_store() {
        let res = send(
            bare_router(),
            Request::builder()
                .uri("/index.html")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(res.headers().get("cache-control").unwrap(), "no-store");
    }

    #[tokio::test]
    async fn embedded_assets_carry_a_strong_etag_and_304() {
        // Any hashed asset from the bundle: immutable + ETag + a matching
        // If-None-Match round-trips to 304 with an empty body.
        let asset = WebAssets::iter()
            .find(|p| p.starts_with("assets/") && p.ends_with(".js"))
            .expect("the bundle carries at least one hashed JS asset");
        let uri = format!("/{asset}");
        let app = bare_router();
        let res = send(
            app,
            Request::builder().uri(&uri).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(res.status(), 200, "{uri} must serve");
        assert_eq!(
            res.headers().get("cache-control").unwrap(),
            "public, max-age=31536000, immutable"
        );
        let etag = res
            .headers()
            .get("etag")
            .expect("embedded files carry a compile-time sha256 ETag")
            .to_str()
            .unwrap()
            .to_owned();

        let app = bare_router();
        let res = send(
            app,
            Request::builder()
                .uri(&uri)
                .header("if-none-match", &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(res.status(), 304);
        assert!(
            axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty(),
            "304 must not ship a body"
        );
    }

    #[tokio::test]
    async fn embedded_lookup_of_a_never_built_name_404s() {
        let res = send(
            bare_router(),
            Request::builder()
                .uri("/assets/definitely-not-a-built-name-0xDEADBEEF.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(res.status(), 404);
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;

    #[test]
    fn traversal_and_absolute_paths_are_rejected() {
        assert_eq!(sanitize("../index.html"), None);
        assert_eq!(sanitize("a/../../secret"), None);
        assert_eq!(sanitize(".."), None);
        assert_eq!(sanitize("."), None);
        assert_eq!(sanitize("/etc/passwd"), None);
        assert_eq!(sanitize(""), None);
        assert_eq!(sanitize("assets/..\0"), None, "NUL bytes never resolve");
    }

    #[test]
    fn plain_relative_paths_survive_unchanged_in_shape() {
        assert_eq!(sanitize("index.html").as_deref(), Some("index.html"));
        assert_eq!(
            sanitize("assets/index-AbCd1234.js").as_deref(),
            Some("assets/index-AbCd1234.js")
        );
    }
}

#[cfg(test)]
mod csp_pairing {
    //! The CSP's inline-script hash and index.html's theme pre-paint
    //! script are a PAIR — edit one, forget the other, and the theme
    //! silently stops applying pre-paint (console error only). This test
    //! cross-checks them against the built dist whenever it exists, so a
    //! drifted hash fails CI instead of surfacing as a user-reported bug.

    #[test]
    fn csp_hash_covers_every_inline_script_in_the_built_index() {
        let dist = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../clients/web/dist/index.html");
        let placeholder = dist.with_file_name("web-ui-placeholder");
        if !dist.is_file() || placeholder.is_file() {
            // No built UI in this checkout (test-only crates, fresh clone),
            // or the build.rs PLACEHOLDER (which has no theme script):
            // nothing to cross-check — the pairing is enforced on real
            // builds. The marker file makes the placeholder machine-readable
            // (a placeholder index.html EXISTS, which is how this test once
            // false-failed on CI).
            eprintln!(
                "skipping: {} not built (placeholder={})",
                dist.display(),
                placeholder.is_file()
            );
            return;
        }
        let Ok(html) = std::fs::read_to_string(&dist) else {
            eprintln!("skipping: {} unreadable", dist.display());
            return;
        };

        // Every inline (src-less) script block in the served page.
        let mut inline = Vec::new();
        let mut rest = html.as_str();
        while let Some(start) = rest.find("<script>") {
            let body = &rest[start + "<script>".len()..];
            let Some(end) = body.find("</script>") else {
                break;
            };
            inline.push(&body[..end]);
            rest = &body[end + "</script>".len()..];
        }
        assert!(
            !inline.is_empty(),
            "expected the theme pre-paint script in index.html"
        );

        for script in inline {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(script.as_bytes());
            let hash = format!(
                "'sha256-{}'",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, digest,)
            );
            assert!(
                crate::web::CSP.contains(&hash),
                "CSP does not cover an inline script (hash {hash}). Recompute the \
                 script-src hash in web.rs after editing index.html's theme script."
            );
        }
    }
}
