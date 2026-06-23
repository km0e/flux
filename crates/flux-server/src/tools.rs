use crate::rpc::make_result;
use rig_core::completion::ToolDefinition;
use serde_json::{json, Value};

pub(crate) fn handle_tools_list(
    id: Option<Value>,
    defs: &[ToolDefinition],
) -> crate::rpc::JsonRpcResponse {
    make_result(id, serde_json::to_value(defs).unwrap_or(json!([])))
}
