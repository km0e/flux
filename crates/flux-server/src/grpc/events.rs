//! The event plane — the session-scoped `Subscribe` server-streaming call,
//! the ONE anchor of a client identity's lifecycle (R3).
//!
//! Opening the stream attaches the identity: the request's `session_id`
//! adopts a previous identity (page refresh / reconnect within the grace
//! window), absence mints a fresh one. The first frame is `ready` carrying
//! the authoritative token plus the leases held after the attach — the
//! old `session_resume` handshake collapsed into this frame. Closing the
//! stream detaches (the stale-teardown guard compares the sink Arc, so a
//! superseded stream's teardown no-ops); the grace window and the reaper
//! take over from there, unchanged.
//!
//! The stream itself IS the identity's sink: a `StreamSink` (two bounded
//! queues — content + priority control) is attached at open, and a pump
//! task drains them into the response in order, control first. A full
//! content queue drops frames (the router's gap machinery recovers); a
//! failed pump send means the client went away — the pump exits and the
//! detach runs. The pump also injects periodic keepalive frames so the
//! client's frame deadline detects a half-open connection (the WS
//! ping/pong role on this plane).

use flux_proto::flux::v1::event_service_server::EventService;
use flux_proto::flux::v1::subscribe_response::Kind;
use flux_proto::flux::v1::{Keepalive, Ready, SubscribeRequest, SubscribeResponse};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tonic::Request;

/// Content queue depth per stream — the fanout's single drop surface.
/// Mirrors the old WS transport's queue: a client that stops reading fills
/// it and further frames are dropped (gap recovery) instead of
/// head-of-line-blocking every chat.
const STREAM_CONTENT_QUEUE: usize = 1024;
/// Control queue depth — must-deliver notices (gap, question prompts) on
/// a small priority queue the pump drains before content.
const STREAM_CONTROL_QUEUE: usize = 16;
/// Server-side keepalive period. The client's deadline is 3× this — a
/// live-but-idle server still marks the connection well within it.
pub(crate) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// The session sink a Subscribe stream exposes to the manager: non-blocking
/// try_send into the two queues (the [`flux_session::SessionSink`] contract,
/// mapped 1:1 onto the stream's plumbing).
struct StreamSink {
    content: mpsc::Sender<SubscribeResponse>,
    control: mpsc::Sender<SubscribeResponse>,
}

impl StreamSink {
    fn new() -> (
        Arc<Self>,
        mpsc::Receiver<SubscribeResponse>,
        mpsc::Receiver<SubscribeResponse>,
    ) {
        let (content_tx, content_rx) = mpsc::channel(STREAM_CONTENT_QUEUE);
        let (control_tx, control_rx) = mpsc::channel(STREAM_CONTROL_QUEUE);
        (
            Arc::new(Self {
                content: content_tx,
                control: control_tx,
            }),
            content_rx,
            control_rx,
        )
    }
}

impl flux_session::SessionSink for StreamSink {
    fn send(&self, event: SubscribeResponse) -> bool {
        use tokio::sync::mpsc::error::TrySendError;
        match self.content.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::warn!("stream content queue full, dropping element");
                false
            }
            Err(TrySendError::Closed(_)) => {
                tracing::debug!("stream gone, dropping element");
                false
            }
        }
    }

    fn send_control(&self, event: SubscribeResponse) -> bool {
        use tokio::sync::mpsc::error::TrySendError;
        match self.control.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                tracing::warn!("stream control queue full, dropping notice");
                false
            }
            Err(TrySendError::Closed(_)) => {
                tracing::debug!("stream gone, dropping notice");
                false
            }
        }
    }
}

/// Drain the two queues into the response stream, control first (the
/// biased select mirrors the WS writer's priority drain). Exits — running
/// the detach teardown — when the response stream dies (client gone), the
/// request future is dropped, or both queues close (the identity dropped
/// this sink). `keepalive` is injectable for tests.
async fn pump(
    mut content: mpsc::Receiver<SubscribeResponse>,
    mut control: mpsc::Receiver<SubscribeResponse>,
    out: mpsc::Sender<Result<SubscribeResponse, tonic::Status>>,
    keepalive: Duration,
) {
    let mut ka = tokio::time::interval_at(tokio::time::Instant::now() + keepalive, keepalive);
    ka.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            control = control.recv() => {
                let Some(el) = control else { break };
                if out.send(Ok(el)).await.is_err() {
                    break;
                }
            }
            content = content.recv() => {
                let Some(el) = content else { break };
                if out.send(Ok(el)).await.is_err() {
                    break;
                }
            }
            _ = ka.tick() => {
                if out
                    .send(Ok(SubscribeResponse {
                        chat_seq: 0,
                        chat_id: String::new(),
                        kind: Some(Kind::Keepalive(Keepalive {})),
                    }))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            else => break,
        }
    }
}

/// The events service. Holds the state so the stream can attach/detach the
/// identity (the lifecycle anchor). The keepalive period is a field —
/// production uses [`KEEPALIVE_INTERVAL`], tests inject a short one.
pub(crate) struct EventPlane {
    state: Arc<flux_session::ServerState>,
    pub(crate) keepalive: Duration,
}

impl EventPlane {
    pub(crate) fn new(state: Arc<flux_session::ServerState>) -> Self {
        Self {
            state,
            keepalive: KEEPALIVE_INTERVAL,
        }
    }
}

#[async_trait::async_trait]
impl EventService for EventPlane {
    type SubscribeStream =
        Pin<Box<dyn futures_util::Stream<Item = Result<SubscribeResponse, tonic::Status>> + Send>>;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<tonic::Response<Self::SubscribeStream>, tonic::Status> {
        let req = request.into_inner();
        let (sink, content_rx, control_rx) = StreamSink::new();
        let (out_tx, out_rx) = mpsc::channel::<Result<SubscribeResponse, tonic::Status>>(64);

        // Attach the identity: adopt the requested token (within the grace
        // window or already live — a duplicate tab sharing it), else mint a
        // fresh one. The ready frame carries the authoritative result in
        // BOTH cases, so the client always overwrites its stored token.
        let (session, leases) = match req.session_id.filter(|s| !s.is_empty()) {
            Some(token) => match self.state.resume_session(&token, sink.clone()).await {
                Some((s, leases)) => (s, leases),
                None => (self.state.attach_session(sink.clone()).await, Vec::new()),
            },
            None => (self.state.attach_session(sink.clone()).await, Vec::new()),
        };
        let token = session.token().to_owned();

        // First frame: the handshake (identity + leases).
        out_tx
            .send(Ok(SubscribeResponse {
                chat_seq: 0,
                chat_id: String::new(),
                kind: Some(Kind::Ready(Ready {
                    session_id: token.clone(),
                    leases,
                })),
            }))
            .await
            .map_err(|_| tonic::Status::internal("stream closed during handshake"))?;

        // The pump owns the stream; when it exits (client gone / request
        // dropped / identity superseded this sink), the detach runs with
        // the SAME sink Arc — the stale-teardown guard makes a superseded
        // stream's teardown a no-op.
        let state = Arc::clone(&self.state);
        let keepalive = self.keepalive;
        let sink: Arc<dyn flux_session::SessionSink> = sink;
        tokio::spawn(async move {
            pump(content_rx, control_rx, out_tx, keepalive).await;
            state.detach_session(&session, &sink).await;
        });

        Ok(tonic::Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(out_rx),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use flux_proto::flux::v1::subscribe_response::Kind as ResponseKind;
    use flux_proto::flux::v1::{SubscribeRequest, SubscribeResponse};
    use flux_proto::prost::Message as _;
    use futures_util::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;

    /// Read gRPC-Web frames off a streaming response until `pred` matches;
    /// returns ALL responses seen (ready included).
    async fn read_until<S, B>(
        stream: &mut S,
        mut pred: impl FnMut(&SubscribeResponse) -> bool,
    ) -> Vec<SubscribeResponse>
    where
        S: futures_util::Stream<Item = reqwest::Result<B>> + Unpin,
        B: AsRef<[u8]>,
    {
        let mut buf: Vec<u8> = Vec::new();
        let mut responses = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                tokio::time::Instant::now() <= deadline,
                "timed out; got {responses:?}"
            );
            while let Some(frame) = take_frame(&mut buf) {
                if let Ok(resp) = SubscribeResponse::decode(&frame[..]) {
                    let matched = pred(&resp);
                    responses.push(resp);
                    if matched {
                        return responses;
                    }
                }
            }
            let chunk = tokio::time::timeout(Duration::from_secs(10), stream.next())
                .await
                .expect("chunk within timeout")
                .expect("stream open")
                .expect("stream ok");
            buf.extend_from_slice(chunk.as_ref());
        }
    }

    /// Poll until `cond` holds (bounded) — the async-side wait helper.
    async fn wait_until(mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "condition not met within 5s"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn take_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
        if buf.len() < 5 {
            return None;
        }
        let len = u32::from_be_bytes(buf[1..5].try_into().unwrap()) as usize;
        if buf.len() < 5 + len {
            return None;
        }
        let frame = buf[5..5 + len].to_vec();
        buf.drain(..5 + len);
        Some(frame)
    }

    /// Open the Subscribe stream over the browser's exact wire; returns the
    /// raw byte stream (the caller decodes with `read_until`).
    async fn open_stream(
        url: &str,
        session_id: Option<String>,
    ) -> impl futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Unpin {
        let req = SubscribeRequest { session_id };
        let resp = web_client()
            .post(format!("{url}/flux.v1.EventService/Subscribe"))
            .header("content-type", "application/grpc-web+proto")
            .header("x-grpc-web", "1")
            .body(lp_frame(&req.encode_to_vec()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        resp.bytes_stream()
    }

    /// A scripted round's text_delta reaches the stream through the
    /// identity's OWN sink — the full loop: stream attach → Connect RPCs
    /// (create + send, token in metadata) → router fanout → stream. No
    /// bridge, no bus: the identity the stream anchored receives its chat's
    /// events because it is a viewer (creation grants the lease +
    /// subscription).
    #[tokio::test]
    async fn subscribe_streams_fanout_events_from_a_scripted_round() {
        let (state, url) = fixture().await;

        // Open the stream BEFORE the round (browser order), read lazily.
        let mut stream = open_stream(&url, None).await;

        // The token comes from the stream's first frame; resolve the SAME
        // identity object it anchored (the ops layer takes handles, not
        // tokens) and drive a scripted round through it.
        let ready = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::Ready(_)))
        })
        .await;
        let token = match ready.first().unwrap().kind.as_ref().unwrap() {
            ResponseKind::Ready(r) => r.session_id.clone(),
            other => panic!("first frame must be ready, got {other:?}"),
        };
        let (session, _leases) = state.session_leases(&token).await.unwrap();
        assert!(!token.is_empty());

        let scripted: Arc<dyn flux_core::Provider> =
            Arc::new(flux_core::test_util::ScriptedProvider::once(vec![vec![
                flux_core::test_util::ScriptItem::Chunk(Ok(flux_core::StreamChunk::Text(
                    "hello plane".into(),
                ))),
                flux_core::test_util::ScriptItem::Chunk(Ok(flux_core::StreamChunk::End {
                    finish_reason: None,
                })),
            ]]));
        let info = state
            .create_chat(
                &session,
                "plane",
                "/tmp",
                flux_core::ChatKind::Classic,
                flux_chat::ResolvedPin {
                    provider: scripted,
                    id: "pinned".into(),
                    model: "m".into(),
                },
            )
            .await
            .unwrap();
        let cid = info.chat_id.to_owned();
        state
            .send_message(&session, &cid, "hi".into())
            .await
            .unwrap();

        let responses = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::StreamEnd(_)))
        })
        .await;

        // The first content frame of the chat carries seq 0 (fetch_add's
        // old value) and the delta content, straight from the router.
        let delta = responses
            .iter()
            .find_map(|r| match &r.kind {
                Some(ResponseKind::TextDelta(t)) => {
                    Some((r.chat_id.as_str(), t.delta.as_str(), r.chat_seq))
                }
                _ => None,
            })
            .expect("text_delta seen");
        assert_eq!(delta.0, cid.as_str());
        assert_eq!(delta.1, "hello plane");
        assert_eq!(delta.2, 0, "the router stamped the frame");
    }

    /// A provider registration broadcasts to every session — the stream
    /// identity receives it through its OWN sink (the global-broadcast
    /// half of the event plane). No tap, no fixture session: the stream
    /// IS a first-class session citizen.
    #[tokio::test]
    async fn provider_broadcast_reaches_the_stream() {
        let (_state, url) = fixture().await;

        let mut stream = open_stream(&url, None).await;
        let ready = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::Ready(_)))
        })
        .await;
        let token = match ready.first().unwrap().kind.as_ref().unwrap() {
            ResponseKind::Ready(r) => r.session_id.clone(),
            other => panic!("first frame must be ready, got {other:?}"),
        };

        let add = flux_proto::flux::v1::AddProviderRequest {
            id: "broadcast-me".into(),
            protocol: "openai".into(),
            url: Some("http://127.0.0.1:1/v1".into()),
            api_key: Some("k".into()),
        };
        let ack = post(
            &url,
            "/flux.v1.ProviderService/AddProvider",
            lp_frame(&add.encode_to_vec()),
            Some(&token),
        )
        .await;
        assert_eq!(ack.status(), 200);

        let responses = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::Providers(p))
                if p.providers.iter().any(|x| x.id == "broadcast-me"))
        })
        .await;
        assert!(
            responses
                .iter()
                .any(|r| matches!(&r.kind, Some(ResponseKind::Providers(p))
                    if p.providers.iter().any(|x| x.id == "broadcast-me"))),
            "the fresh registry arrived on the stream"
        );
    }

    /// The stream open IS the adoption: a detached identity's token in the
    /// request adopts it — the ready frame carries the SAME token plus the
    /// leases held, and the identity is live again.
    #[tokio::test]
    async fn stream_open_adopts_a_detached_identity() {
        let (state, url) = fixture().await;

        // First stream: mint an identity, grant it a lease (create a chat),
        // then drop the stream (page refresh analog — the client detaches).
        let mut first = open_stream(&url, None).await;
        let ready = read_until(&mut first, |r| {
            matches!(&r.kind, Some(ResponseKind::Ready(_)))
        })
        .await;
        let token = match ready.first().unwrap().kind.as_ref().unwrap() {
            ResponseKind::Ready(r) => r.session_id.clone(),
            other => panic!("first frame must be ready, got {other:?}"),
        };
        drop(first);
        let (session, _leases) = state.session_leases(&token).await.unwrap();
        let scripted: Arc<dyn flux_core::Provider> = Arc::new(flux_core::test_util::DummyProvider);
        let info = state
            .create_chat(
                &session,
                "c",
                "/tmp",
                flux_core::ChatKind::Classic,
                flux_chat::ResolvedPin {
                    provider: scripted,
                    id: "pinned".into(),
                    model: "m".into(),
                },
            )
            .await
            .unwrap();
        // Detach completes (the pump observed the drop) — grace window.
        wait_until(|| !session.is_live()).await;
        assert!(!session.is_live(), "dropping the stream must detach");
        // Still resolvable within the grace window (the adoptability rule).
        assert!(state.session_leases(&token).await.is_some());

        // Second stream with the token: adopt — same token, lease reported.
        let mut second = open_stream(&url, Some(token.clone())).await;
        let ready = read_until(&mut second, |r| {
            matches!(&r.kind, Some(ResponseKind::Ready(_)))
        })
        .await;
        match ready.first().unwrap().kind.as_ref().unwrap() {
            ResponseKind::Ready(r) => {
                assert_eq!(r.session_id, token, "the token was adopted");
                assert_eq!(r.leases, vec![info.chat_id.to_string()]);
            }
            other => panic!("expected ready, got {other:?}"),
        }
    }

    /// An unknown token mints a fresh identity — the ready frame carries a
    /// DIFFERENT token (the client overwrites its stored id).
    #[tokio::test]
    async fn unknown_token_mints_fresh() {
        let (_state, url) = fixture().await;
        let mut stream = open_stream(&url, Some("not-a-token".into())).await;
        let ready = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::Ready(_)))
        })
        .await;
        match ready.first().unwrap().kind.as_ref().unwrap() {
            ResponseKind::Ready(r) => {
                assert_ne!(r.session_id, "not-a-token");
                assert!(r.leases.is_empty());
            }
            other => panic!("expected ready, got {other:?}"),
        }
    }

    /// A chat claimed through the stream's identity delivers its snapshot
    /// ON the stream (single-point delivery: history + state ride the
    /// identity sink, which IS the stream), peeking the seq.
    #[tokio::test]
    async fn claim_snapshot_rides_the_stream() {
        let (state, url) = fixture().await;
        let mut stream = open_stream(&url, None).await;
        let ready = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::Ready(_)))
        })
        .await;
        let token = match ready.first().unwrap().kind.as_ref().unwrap() {
            ResponseKind::Ready(r) => r.session_id.clone(),
            other => panic!("first frame must be ready, got {other:?}"),
        };
        let (session, _leases) = state.session_leases(&token).await.unwrap();
        let scripted: Arc<dyn flux_core::Provider> = Arc::new(flux_core::test_util::DummyProvider);
        let info = state
            .create_chat(
                &session,
                "c",
                "/tmp",
                flux_core::ChatKind::Classic,
                flux_chat::ResolvedPin {
                    provider: scripted,
                    id: "pinned".into(),
                    model: "m".into(),
                },
            )
            .await
            .unwrap();
        state
            .store
            .append_messages(&info.chat_id, &[flux_core::Message::user("hi")])
            .await
            .unwrap();
        // Release, then re-claim through the Connect surface (the stream
        // identity, token in metadata) — a GRANT delivers the snapshot on
        // the STREAM (the already-owned fast path skips re-delivery).
        state.release_chat(&session, &info.chat_id).await;
        let resp = post(
            &url,
            "/flux.v1.ChatService/ClaimChat",
            lp_frame(
                &flux_proto::flux::v1::ClaimChatRequest {
                    chat_id: info.chat_id.to_string(),
                }
                .encode_to_vec(),
            ),
            Some(&token),
        )
        .await;
        assert_eq!(resp.status(), 200);

        let responses = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::ChatState(_)))
        })
        .await;
        let hist = responses
            .iter()
            .find(|r| matches!(&r.kind, Some(ResponseKind::ChatHistory(_))))
            .expect("history snapshot on the stream");
        assert_eq!(hist.chat_id, info.chat_id.to_string());
        match &hist.kind {
            Some(ResponseKind::ChatHistory(h)) => {
                assert_eq!(h.messages.len(), 1);
                assert_eq!(h.messages[0].content, "hi");
            }
            other => panic!("expected chat_history, got {other:?}"),
        }
        let state_el = responses
            .iter()
            .find(|r| matches!(&r.kind, Some(ResponseKind::ChatState(_))))
            .unwrap();
        // The snapshots peek the CURRENT seq (0 on a fresh chat) — later
        // events carry the same or greater values (never below).
        assert_eq!(state_el.chat_seq, 0);
    }

    /// The stream close feeds the reaper: after the grace window lapses
    /// (shortened), the reaper releases the leases and drops the identity.
    #[tokio::test]
    async fn stream_close_feeds_the_reaper() {
        // Short keepalive: a vanished client is noticed at the pump's next
        // write (the keepalive period bounds the detach latency).
        let (state, url) = fixture_with_keepalive(Duration::from_millis(150)).await;
        let mut stream = open_stream(&url, None).await;
        let ready = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::Ready(_)))
        })
        .await;
        let token = match ready.first().unwrap().kind.as_ref().unwrap() {
            ResponseKind::Ready(r) => r.session_id.clone(),
            other => panic!("first frame must be ready, got {other:?}"),
        };
        let (session, _leases) = state.session_leases(&token).await.unwrap();
        drop(stream);
        wait_until(|| !session.is_live()).await;
        state.set_grace(std::time::Duration::ZERO);
        state.reap_detached().await;
        assert!(
            state.session_leases(&token).await.is_none(),
            "the reaper released the reaped identity"
        );
    }

    /// The keepalive: the pump injects periodic liveness frames on the
    /// session level (empty chat_id, chat_seq 0) — the client's frame
    /// deadline keys on ANY element (keepalive included) to detect a
    /// half-open connection. The fixture's interval is short.
    #[tokio::test]
    async fn keepalive_frames_arrive_periodically() {
        let (_state, url) = fixture_with_keepalive(Duration::from_millis(150)).await;
        let mut stream = open_stream(&url, None).await;
        let responses = read_until(&mut stream, |r| {
            matches!(&r.kind, Some(ResponseKind::Keepalive(_)))
        })
        .await;
        assert!(
            responses.len() >= 2,
            "ready + at least one keepalive must have arrived"
        );
        assert!(matches!(
            &responses.first().unwrap().kind,
            Some(ResponseKind::Ready(_))
        ));
        let ka = responses.last().unwrap();
        assert_eq!(ka.chat_id, "");
        assert_eq!(ka.chat_seq, 0);
    }
}
