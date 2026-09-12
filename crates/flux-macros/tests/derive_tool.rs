//! Tests for the `#[derive(Tool)]` proc-macro.
//!
//! The macro generates `name()`, `description()`, `schema()` (inferred from
//! struct fields) and `call()` (converts the processed `HashMap<String,
//! Value>` into a JSON object, deserializes into `Self`, then delegates to
//! `execute()`).
#![allow(dead_code)]

use flux_core::CoreError;
use flux_core::Tool as _;
use flux_macros::Tool;
use serde_json::json;
use std::collections::HashMap;

// ── Basic derive: string field ──

#[derive(Tool, ::serde::Deserialize)]
#[tool(name = "echo", description = "Echoes input")]
struct EchoTool {
    /// The message to echo.
    message: String,
}

impl EchoTool {
    async fn execute(&self, _ctx: flux_core::ToolCtx) -> Result<String, CoreError> {
        Ok(self.message.clone())
    }
}

#[test]
fn basic_metadata() {
    let t = EchoTool {
        message: "hi".into(),
    };
    assert_eq!(t.name(), "echo");
    assert_eq!(t.description(), "Echoes input");
}

#[test]
fn basic_schema_has_field() {
    let t = EchoTool {
        message: "hi".into(),
    };
    let schema = t.schema();
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("message"));
    assert_eq!(props["message"]["type"], "string");
    assert_eq!(props["message"]["description"], "The message to echo.");
    // message is not Option, so it should be in required.
    let required: Vec<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"message"));
}

#[tokio::test]
async fn call_deserializes_arguments_into_self() {
    let t = EchoTool {
        message: "initial".into(),
    };
    // call() must rebuild `self` from the arguments, so the result comes
    // from the args rather than the receiver's original field value.
    let result = t
        .call(
            HashMap::from([("message".to_string(), json!("from args"))]),
            flux_core::ToolCtx::new(),
        )
        .await;
    assert_eq!(result.unwrap(), "from args");
}

#[tokio::test]
async fn call_rejects_missing_required_field() {
    let t = EchoTool {
        message: "x".into(),
    };
    let err = t
        .call(HashMap::new(), flux_core::ToolCtx::new())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::InvalidArguments(_)),
        "missing field should be an InvalidArguments error"
    );
}

#[tokio::test]
async fn call_rejects_wrong_field_type() {
    let t = EchoTool {
        message: "x".into(),
    };
    // Strongly-typed fields remain: a String field receiving a number is
    // still InvalidArguments.
    let err = t
        .call(
            HashMap::from([("message".to_string(), json!(42))]),
            flux_core::ToolCtx::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::InvalidArguments(_)),
        "wrong type should be an InvalidArguments error"
    );
}

// ── Option field ──

#[derive(Tool, ::serde::Deserialize)]
#[tool(name = "opt_tool", description = "Has optional field")]
struct OptTool {
    /// Required field.
    required_field: String,
    /// An optional field.
    optional_field: Option<String>,
}

impl OptTool {
    async fn execute(&self, _ctx: flux_core::ToolCtx) -> Result<String, CoreError> {
        Ok("ok".into())
    }
}

#[test]
fn option_not_in_required() {
    let t = OptTool {
        required_field: "x".into(),
        optional_field: None,
    };
    let schema = t.schema();
    let required: Vec<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"required_field"));
    assert!(!required.contains(&"optional_field"));
}

#[tokio::test]
async fn call_accepts_missing_optional_field() {
    let t = OptTool {
        required_field: "x".into(),
        optional_field: None,
    };
    let result = t
        .call(
            HashMap::from([("required_field".to_string(), json!("y"))]),
            flux_core::ToolCtx::new(),
        )
        .await;
    assert_eq!(result.unwrap(), "ok");
}

// ── serde(default) detection ──

#[derive(Tool, ::serde::Deserialize)]
#[tool(name = "default_tool", description = "Tests serde default handling")]
struct DefaultFieldTool {
    /// serde(default) — not required.
    #[serde(default)]
    defaulted: String,
    /// Rename containing the substring "default" — must NOT be mistaken
    /// for `#[serde(default)]` (C8).
    #[serde(rename = "default_name")]
    renamed: String,
    /// Plain required field.
    plain: String,
}

impl DefaultFieldTool {
    async fn execute(&self, _ctx: flux_core::ToolCtx) -> Result<String, CoreError> {
        Ok("ok".into())
    }
}

#[test]
fn rename_containing_default_keeps_field_required() {
    let t = DefaultFieldTool {
        defaulted: String::new(),
        renamed: "r".into(),
        plain: "p".into(),
    };
    let schema = t.schema();
    let required: Vec<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        required.contains(&"renamed"),
        "rename = \"default_name\" must not flip the field out of required"
    );
    assert!(required.contains(&"plain"));
    assert!(
        !required.contains(&"defaulted"),
        "serde(default) stays optional"
    );
}

// ── #[tool(skip)] ──

#[derive(Tool, ::serde::Deserialize)]
#[tool(name = "skip_tool", description = "Tests skip attribute")]
struct SkipTool {
    /// Visible field.
    visible: String,
    #[tool(skip)]
    #[allow(dead_code)]
    internal: String,
}

impl SkipTool {
    async fn execute(&self, _ctx: flux_core::ToolCtx) -> Result<String, CoreError> {
        Ok("ok".into())
    }
}

#[test]
fn skip_excludes_field_from_schema() {
    let t = SkipTool {
        visible: "v".into(),
        internal: "secret".into(),
    };
    let schema = t.schema();
    let props = schema["properties"].as_object().unwrap();
    assert!(props.contains_key("visible"));
    assert!(
        !props.contains_key("internal"),
        "internal should be excluded"
    );
}

// ── #[tool(required)] ──

#[derive(Tool, ::serde::Deserialize)]
#[tool(name = "req_tool", description = "Tests required override")]
struct ReqTool {
    always: String,
    #[tool(required)]
    forced: Option<String>,
}

impl ReqTool {
    async fn execute(&self, _ctx: flux_core::ToolCtx) -> Result<String, CoreError> {
        Ok("ok".into())
    }
}

#[test]
fn required_override() {
    let t = ReqTool {
        always: "x".into(),
        forced: None,
    };
    let schema = t.schema();
    let required: Vec<_> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"always"));
    assert!(
        required.contains(&"forced"),
        "forced should be required despite Option"
    );
}

// ── Vec field ──

#[derive(Tool, ::serde::Deserialize)]
#[tool(name = "vec_tool", description = "Has array field")]
struct VecTool {
    /// List of items.
    items: Vec<String>,
}

impl VecTool {
    async fn execute(&self, _ctx: flux_core::ToolCtx) -> Result<String, CoreError> {
        Ok("ok".into())
    }
}

#[test]
fn vec_field_has_array_type() {
    let t = VecTool { items: vec![] };
    let schema = t.schema();
    let item_schema = &schema["properties"]["items"];
    assert_eq!(item_schema["type"], "array");
    assert_eq!(item_schema["items"]["type"], "string");
}
