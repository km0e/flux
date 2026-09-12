//! Minimal Server-Sent Events client.

use async_stream::stream;
use bytes::Bytes;
use flux_core::CoreError;
use futures::{Stream, StreamExt};
use std::error::Error;
use std::pin::Pin;
use tracing::{debug, trace, warn};

/// Lightweight SSE-over-HTTP client.  Handles POST + `text/event-stream` parsing.
pub struct SseClient {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl SseClient {
    pub fn new(client: reqwest::Client, endpoint: String, api_key: String) -> Self {
        Self {
            client,
            endpoint,
            api_key,
        }
    }

    /// POST `body` to the configured endpoint and return a stream of SSE
    /// `data:` payloads. `[DONE]` is NOT intercepted here — the semantic
    /// call belongs to the consumer (openai.rs uses it to tell a normal
    /// end from an upstream truncation).
    pub async fn stream(
        &self,
        body: String,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<String, CoreError>> + Send>>, CoreError> {
        debug!(endpoint = %self.endpoint, "sending request to upstream");
        let response = match self
            .client
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let mut detail = format!("{e}");
                if let Some(source) = e.source() {
                    detail.push_str(" | caused by: ");
                    detail.push_str(&source.to_string());
                }
                warn!(endpoint = %self.endpoint, error = %detail, "failed to send request to upstream");
                return Err(CoreError::Provider(format!(
                    "HTTP request failed: {detail}"
                )));
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            // Size-cap the error body read (truncated at 4KB) — never pull a
            // huge upstream error response fully into memory or the message.
            let mut buf: Vec<u8> = Vec::new();
            let mut bytes = response.bytes_stream();
            while let Some(chunk) = bytes.next().await {
                let Ok(chunk) = chunk else { break };
                if buf.len() >= 4096 {
                    buf.extend_from_slice(b"...(truncated)");
                    break;
                }
                let take = (4096 - buf.len()).min(chunk.len());
                buf.extend_from_slice(&chunk[..take]);
            }
            let text = String::from_utf8_lossy(&buf);
            warn!(%status, %text, "upstream error");
            return Err(CoreError::Provider(format!("HTTP {status}: {text}")));
        }

        trace!(status = %response.status(), "upstream connected");

        Ok(sse_stream(response.bytes_stream()))
    }
}

/// Maximum bytes buffered across the SSE line remainder and the in-progress
/// data payload. A well-behaved SSE stream emits `data:` lines and blank-line
/// terminators, so this stays near the largest single line; an uncooperative
/// or faulty upstream that never sends a newline (or never terminates a
/// payload) must not accumulate unbounded memory.
const MAX_SSE_BUFFER: usize = 16 * 1024 * 1024;

/// Parse `text/event-stream` byte chunks into individual `data:` payloads.
fn sse_stream(
    bytes: impl Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
) -> Pin<Box<dyn Stream<Item = Result<String, CoreError>> + Send>> {
    let mut remainder: Vec<u8> = Vec::new();
    let mut data_buf = String::new();

    Box::pin(stream! {
        for await chunk in bytes {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    yield Err(CoreError::Provider(format!("SSE read error: {e}")));
                    return;
                }
            };
            remainder.extend_from_slice(&chunk);

            // Bounded buffering: a stream that never produces a newline (or a
            // payload terminator) within the cap is presumed broken — fail
            // rather than accumulate unbounded memory.
            if remainder.len() > MAX_SSE_BUFFER {
                yield Err(CoreError::Provider(format!(
                    "SSE line buffer exceeded {MAX_SSE_BUFFER} bytes without a newline"
                )));
                return;
            }

            // Split lines on raw bytes: a multi-byte UTF-8 character
            // straddling two network chunks is only decoded once its full
            // line has arrived, so it can never be corrupted to U+FFFD.
            while let Some(newline) = remainder.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8_lossy(&remainder[..newline]).into_owned();
                remainder.drain(..=newline);
                let line = line.trim_end_matches('\r');
                // Non-standard gateway compat: strip a UTF-8 BOM (first line) + case-insensitive `data:`.
                let line = line.trim_start_matches('\u{feff}');

                if line.is_empty() {
                    if !data_buf.is_empty() {
                        yield Ok(std::mem::take(&mut data_buf));
                    }
                } else if line.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:")) {
                    let data = &line[5..];
                    let data = data.strip_prefix(' ').unwrap_or(data);
                    let add = data.len() + if data_buf.is_empty() { 0 } else { 1 };
                    if data_buf.len() + add > MAX_SSE_BUFFER {
                        yield Err(CoreError::Provider(format!(
                            "SSE payload exceeded {MAX_SSE_BUFFER} bytes"
                        )));
                        return;
                    }
                    if !data_buf.is_empty() { data_buf.push('\n'); }
                    data_buf.push_str(data);
                }
            }
        }
        // EOF flush: a provider closing the stream right after the last
        // `data:` line (no terminating blank line) must not lose the event.
        if !data_buf.is_empty() {
            yield Ok(std::mem::take(&mut data_buf));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::stream;

    fn ok(bytes: &'static [u8]) -> reqwest::Result<Bytes> {
        Ok(Bytes::from_static(bytes))
    }

    #[tokio::test]
    async fn multibyte_char_split_across_chunks_is_not_corrupted() {
        // '你' = E4 BD A0 — split after E4 BD; '好' = E5 A5 BD arrives whole
        // in the second chunk. Per-chunk lossy decoding turns the split char
        // into U+FFFD replacement garbage.
        let input = stream::iter(vec![
            ok(b"data: {\"content\":\"\xE4\xBD"),
            ok(b"\xA0\xE5\xA5\xBD\"}\n\n"),
        ]);
        let mut parsed = sse_stream(input);

        let event = parsed
            .next()
            .await
            .expect("expected one SSE event")
            .expect("expected a parseable event");
        assert!(event.contains("你好"), "multibyte char corrupted: {event}");
        assert!(parsed.next().await.is_none(), "unexpected trailing events");
    }

    #[tokio::test]
    async fn ascii_lines_split_across_chunks_are_reassembled() {
        let input = stream::iter(vec![
            ok(b"data: {\"content\":\"he"),
            ok(b"llo\"}\n"),
            ok(b"\n"),
        ]);
        let mut parsed = sse_stream(input);

        let event = parsed
            .next()
            .await
            .expect("expected one SSE event")
            .expect("expected a parseable event");
        assert!(event.contains("hello"), "line split corrupted: {event}");
    }

    #[tokio::test]
    async fn bom_prefixed_and_uppercase_data_lines_are_parsed() {
        // Non-standard gateway compat: a leading UTF-8 BOM and an uppercase `DATA:` prefix must both parse.
        let input = stream::iter(vec![
            ok(b"\xEF\xBB\xBFDATA: {\"content\":\"bom\"}\n\n"),
            ok(b"Data: {\"content\":\"mixed\"}\n\n"),
        ]);
        let mut parsed = sse_stream(input);

        let first = parsed
            .next()
            .await
            .expect("expected first event")
            .expect("expected a parseable event");
        assert!(first.contains("bom"), "BOM-prefixed line failed: {first}");
        let second = parsed
            .next()
            .await
            .expect("expected second event")
            .expect("expected a parseable event");
        assert!(second.contains("mixed"), "mixed-case line failed: {second}");
        assert!(parsed.next().await.is_none(), "unexpected trailing events");
    }

    #[tokio::test]
    async fn final_event_without_trailing_blank_line_is_flushed() {
        // A provider closing the stream right after the last data line must
        // not lose the final event.
        let input = stream::iter(vec![ok(b"data: {\"content\":\"bye\"}\n")]);
        let mut parsed = sse_stream(input);

        let event = parsed
            .next()
            .await
            .expect("final event flushed on EOF")
            .expect("expected a parseable event");
        assert!(event.contains("bye"), "final event corrupted: {event}");
        assert!(parsed.next().await.is_none(), "unexpected trailing events");
    }

    #[tokio::test]
    async fn unbounded_line_without_newline_is_rejected() {
        // An upstream that never emits a newline must fail once the line
        // buffer exceeds the cap instead of accumulating unbounded memory.
        let chunks = stream::repeat_with(|| ok(&[b'x'; 8192])).take(MAX_SSE_BUFFER / 8192 + 2);
        let mut parsed = sse_stream(chunks);
        let first = parsed.next().await.expect("expected a terminal error");
        assert!(
            matches!(first, Err(CoreError::Provider(_))),
            "unbounded line buffer must be rejected, got {first:?}"
        );
        // The stream terminates after the error.
        assert!(parsed.next().await.is_none());
    }

    #[tokio::test]
    async fn unbounded_payload_across_data_lines_is_rejected() {
        // Multiple `data:` lines joined into one payload that never gets a
        // blank-line terminator must also be bounded.
        let chunks = stream::repeat_with(|| ok(b"data: yyyyyyyy\n")).take(MAX_SSE_BUFFER + 1);
        let mut parsed = sse_stream(chunks);
        let mut saw_err = false;
        while let Some(item) = parsed.next().await {
            if item.is_err() {
                saw_err = true;
                break;
            }
        }
        assert!(saw_err, "unbounded payload must be rejected");
    }
}
