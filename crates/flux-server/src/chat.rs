use crate::agent::DynPrompt;
use crate::rpc::{make_error, make_result};
use rig_core::completion::Message;
use serde_json::{json, Value};

pub(crate) async fn handle_chat(
    id: Option<Value>,
    params: Option<Value>,
    agent: &dyn DynPrompt,
    history: &mut Vec<Message>,
) -> crate::rpc::JsonRpcResponse {
    let user_message = params
        .as_ref()
        .and_then(|p| p.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    match agent.chat(user_message, history).await {
        Ok(content) => make_result(
            id,
            json!({
                "role": "assistant",
                "content": content,
                "tool_calls": []
            }),
        ),
        Err(e) => make_error(id, -32603, e.to_string(), None),
    }
}
