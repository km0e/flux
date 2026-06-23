use crate::agent::DynStreamingPrompt;
use crate::rpc::JsonRpcNotification;
use crate::state::ServerState;
use crate::transport::Notifier;
use futures::StreamExt;
use rig_core::completion::Message;
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Events emitted by a streaming agent response.
#[derive(Debug)]
pub(crate) enum StreamEvent {
    /// A new text delta.
    Chunk(String),
    /// The final concatenated response and any new history messages.
    Done(String, Vec<Message>),
}

pub(crate) async fn run_stream<N: Notifier>(
    agent: Arc<dyn DynStreamingPrompt>,
    state: std::sync::Arc<tokio::sync::Mutex<ServerState>>,
    stream_id: String,
    message: String,
    notifier: N,
) {
    info!(stream_id = %stream_id, message_len = message.len(), "starting stream");
    let history = {
        let st = state.lock().await;
        st.history.clone()
    };
    match agent.stream_chat(&message, history).await {
        Ok(mut stream) => {
            let mut chunks = 0usize;
            while let Some(event) = stream.next().await {
                match event {
                    StreamEvent::Chunk(delta) => {
                        chunks += 1;
                        debug!(stream_id = %stream_id, chunk = chunks, delta_len = delta.len(), "stream chunk");
                        notifier
                            .send_json(JsonRpcNotification {
                                jsonrpc: "2.0".to_string(),
                                method: "stream/chunk".to_string(),
                                params: json!({ "stream_id": stream_id, "delta": delta }),
                            })
                            .await;
                    }
                    StreamEvent::Done(full, new_history) => {
                        info!(stream_id = %stream_id, chunks, "stream done");
                        if let Ok(mut st) = state.try_lock() {
                            st.history.extend(new_history);
                        } else {
                            warn!(stream_id = %stream_id, "failed to lock state to save history");
                        }
                        notifier
                            .send_json(JsonRpcNotification {
                                jsonrpc: "2.0".to_string(),
                                method: "stream/done".to_string(),
                                params: json!({ "stream_id": stream_id, "content": full }),
                            })
                            .await;
                        return;
                    }
                }
            }
            info!(stream_id = %stream_id, chunks, "stream ended without final content");
            notifier
                .send_json(JsonRpcNotification {
                    jsonrpc: "2.0".to_string(),
                    method: "stream/done".to_string(),
                    params: json!({ "stream_id": stream_id, "content": "" }),
                })
                .await;
        }
        Err(e) => {
            warn!(stream_id = %stream_id, error = %e, "stream error");
            notifier
                .send_json(JsonRpcNotification {
                    jsonrpc: "2.0".to_string(),
                    method: "stream/error".to_string(),
                    params: json!({ "stream_id": stream_id, "message": e.to_string() }),
                })
                .await;
        }
    }
}
