//! Browser UI static site — served by the SAME axum server as the
//! Connect surface (`/flux.v1.*`) and the terminal side channel
//! (`/ws/term`) — one process, one port. The page is a viewer/controller
//! that connects back over the same origin, so the static layer stays a
//! pure leaf — it never proxies anything.
//!
//! Header policy split, all crate-native (tower-http):
//! - Cache-Control is PER-ROUTE policy: hashed assets get `immutable`
//!   (`SetResponseHeaderLayer` on the asset service — ServeDir deliberately
//!   does not manage caching), index.html gets `no-store` inline (it must
//!   revalidate to see new asset hashes after a rebuild).
//! - Security headers are UNIFORM policy: one CSP header + nosniff via two
//!   `SetResponseHeaderLayer`s composed by `ServiceBuilder`, applied over
//!   every route AND the fallback. `img-src 'self'` is load-bearing: the
//!   page runs on plain http, so same-origin images (the favicon) need it —
//!   `https:` alone does not match them.
//! - The body-mechanics (Content-Length vs chunked, Vary, conditional
//!   requests) stay entirely inside the crates: CompressionLayer owns
//!   transfer headers and skips 304s/empty bodies; the header layers only
//!   touch their own headers.
//!
//! Security posture: the server acts with the starting user's
//! permissions; the port is bound to 127.0.0.1 by default. Remote exposure
//! = reverse proxy with TLS, an explicit user decision.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::path::{Path, PathBuf};
use tower::{Layer as _, ServiceBuilder};
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

/// CSP for the served page — strict posture: nothing but the app's own
/// hashed assets and its WebSocket.
///
/// `script-src` carries the sha256 of the ONE inline script the build
/// emits: index.html's theme pre-paint block (applies the persisted
/// dark/light choice before first paint — a hash instead of
/// 'unsafe-inline' keeps the XSS bar high). If that script ever changes,
/// recompute: `openssl dgst -sha256 -binary | openssl base64` over the
/// exact bytes between `<script>` and `</script>` (the ws_e2e-adjacent
/// unit test below cross-checks it against the built dist when present).
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

/// The static-site routes (`/`, `/index.html`, `/manifest.webmanifest`,
/// `/assets/*`, 404 fallback), ready to merge into the transport router.
/// No startup validation: an
/// incomplete build surfaces at request time — index reads 500 + a warn
/// log, missing assets 404 — a deliberate tradeoff (see decisions.md
/// T-06): the server must not refuse to boot over UI assets.
///
/// Why per-route instead of one `ServeDir::new(root)`: the cache-policy
/// boundary is irreducible. Hashed assets cache `immutable` (reloads
/// transfer nothing), but `index.html` MUST revalidate (`no-store`) — it
/// references the hashed names, and a cached index would point at old
/// hashes after a rebuild (a white page). Either way the path branch
/// exists; here it is explicit. The root dir is the only state;
/// `index.html` is read per request (async, on the blocking pool) so a
/// rebuild between page loads is picked up without a restart — the
/// `no-store` contract keeps its promise.
pub(crate) fn router(root: &Path) -> Router {
    let assets_root = root.join("assets");

    // The asset service: compression (outermost) + immutable cache-control
    // wrapped around ServeDir via the Layer trait directly — MethodRouter's
    // layered-error inference fights a two-layer route stack.
    let assets = CompressionLayer::new().layer(
        SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static(ASSET_CACHE),
        )
        .layer(ServeDir::new(&assets_root).append_index_html_on_directories(false)),
    );

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
        // nest_service strips the /assets prefix before dispatching, so
        // ServeDir resolves names exactly as the build emitted them.
        .nest_service("/assets", assets)
        .fallback(not_found)
        .layer(security)
        .with_state(root.to_path_buf())
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

/// The index page. Cache-Control `no-store`: after a rebuild the asset
/// hashes change, and the page must never serve a stale HTML that points at
/// them. Read per request (tokio::fs → blocking pool) so a rebuild is
/// picked up without a server restart; a vanished file (racing rebuild)
/// degrades to a 500 with a plain note.
async fn serve_index(State(root): State<PathBuf>) -> Response {
    serve_dist_file(
        root,
        "index.html",
        "text/html; charset=utf-8",
        "index.html unavailable\n",
    )
    .await
}

/// The PWA webmanifest — same contract as the index page: read per request
/// so a rebuild is picked up without a restart, `no-store` so a stale
/// manifest never outlives its build.
async fn serve_manifest(State(root): State<PathBuf>) -> Response {
    serve_dist_file(
        root,
        "manifest.webmanifest",
        "application/manifest+json",
        "manifest unavailable\n",
    )
    .await
}

/// Shared per-request small-file service for the unhashed dist-root files
/// (index.html, manifest.webmanifest): read on the async fs, `no-store`,
/// and a plain 500 when the file vanished mid-rebuild.
async fn serve_dist_file(
    root: PathBuf,
    name: &str,
    content_type: &'static str,
    vanished: &'static str,
) -> Response {
    match tokio::fs::read_to_string(root.join(name)).await {
        Ok(body) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(e) => {
            tracing::warn!(error = %e, file = name, "dist file vanished at request time");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                vanished,
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// A minimal but complete build layout in a temp dir.
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
        (router(dir.path()), dir)
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
        // A path that never appeared in the build 404s too — ServeDir's
        // path resolution is the traversal guard.
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
        let Ok(html) = std::fs::read_to_string(&dist) else {
            // No built UI in this checkout (test-only crates, fresh clone):
            // nothing to cross-check — the pairing is enforced on builds.
            eprintln!("skipping: {} not built", dist.display());
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
