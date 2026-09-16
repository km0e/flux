//! Model-catalog tests: `list_models` against a canned local HTTP server
//! (no real upstream — the wire shape and error mapping are what matter).

// Hermeticity guard — this file is a SEPARATE test binary (the lib's ctor
// does not cover it); see flux-test-support's docs.
flux_test_support::test_env_guard!();

use flux_core::Provider;
use flux_provider::{OpenAiConfig, OpenAiProvider};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve ONE canned HTTP response, return its base URL. The content-length
/// header is computed from the body so reqwest never sees a short read.
async fn serve_one(status: &str, body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (status, body) = (status.to_string(), body.to_string());
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf).await; // the request (ignored)
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        sock.write_all(head.as_bytes()).await.unwrap();
        sock.write_all(body.as_bytes()).await.unwrap();
        sock.shutdown().await.unwrap();
    });
    format!("http://{addr}/v1")
}

fn provider(base_url: String) -> OpenAiProvider {
    OpenAiProvider::new(
        OpenAiConfig {
            base_url,
            api_key: Some("sk-test".into()),
        },
        // The probe is model-agnostic — any pin works.
        "gpt-4o-mini".into(),
        // The /models probe never carries generation params.
        flux_provider::OpenAiParams::default(),
        // Proxy-proof: a host `http_proxy` must never detour loopback.
        flux_test_support::loopback_client_builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn list_models_parses_and_sorts_ids() {
    let base = serve_one(
        "200 OK",
        r#"{"data":[{"id":"gpt-4o"},{"id":"gpt-3.5-turbo"},{"id":"o3-mini"}]}"#,
    )
    .await;
    let models = provider(base).list_models().await.unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["gpt-3.5-turbo", "gpt-4o", "o3-mini"], "sorted");
}

#[tokio::test]
async fn list_models_carries_context_length_from_common_extras() {
    // Each gateway flavor names the field differently; all are optional and
    // a standard payload carries none.
    let base = serve_one(
        "200 OK",
        r#"{"data":[
            {"id":"standard-model"},
            {"id":"vllm-model","max_model_len":32768},
            {"id":"groq-model","context_window":131072},
            {"id":"openrouter-model","context_length":1000000},
            {"id":"openrouter-nested","top_provider":{"context_length":200000}},
            {"id":"precedence","context_length":111,"max_model_len":222}
        ]}"#,
    )
    .await;
    let models = provider(base).list_models().await.unwrap();
    let ctx = |id: &str| {
        models
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.context_length)
            .unwrap_or_else(|| panic!("{id} missing"))
    };
    assert_eq!(ctx("standard-model"), None, "standard payload: no guess");
    assert_eq!(ctx("vllm-model"), Some(32768));
    assert_eq!(ctx("groq-model"), Some(131072));
    assert_eq!(ctx("openrouter-model"), Some(1000000));
    assert_eq!(ctx("openrouter-nested"), Some(200000));
    assert_eq!(ctx("precedence"), Some(111), "flat key wins over fallbacks");
}

#[tokio::test]
async fn list_models_maps_http_errors_to_provider_errors() {
    let base = serve_one("401 Unauthorized", "").await;
    let err = provider(base).list_models().await.unwrap_err();
    assert!(err.to_string().contains("401"), "{err}");
}

#[tokio::test]
async fn list_models_maps_bad_json_to_provider_errors() {
    let base = serve_one("200 OK", "not{}").await;
    let err = provider(base).list_models().await.unwrap_err();
    assert!(err.to_string().contains("parse"), "{err}");
}
