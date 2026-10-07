//! Minimal JSON-RPC 2.0 plumbing for MCP.
//!
//! MCP frames are newline-delimited JSON objects over a byte stream. We only
//! need a handful of methods, so we model just those rather than pulling in a
//! full protocol framework.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// JSON-RPC version marker. Every message we emit or accept uses it.
pub const JSONRPC_VERSION: &str = "2.0";

/// A request or notification travelling in either direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Message {
    /// Build a request that expects a matching response.
    pub fn request(id: Value, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id),
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    /// Build a notification, which by definition has no response.
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: None,
            method: Some(method.into()),
            params: Some(params),
            result: None,
            error: None,
        }
    }

    /// Build a successful response to a request with the given id.
    ///
    /// Takes `Option<Value>` because a JSON-RPC id may legitimately be absent
    /// or null when the request was too malformed to recover one.
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id.unwrap_or(Value::Null)),
            method: None,
            params: None,
            result: Some(result),
            error: None,
        }
    }

    /// Build an error response. Per JSON-RPC, `id` is null when the request
    /// could not be parsed far enough to recover an id.
    pub fn failure(id: Option<Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id.unwrap_or(Value::Null)),
            method: None,
            params: None,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    /// Whether this message is a request or notification (i.e. needs a reply).
    pub fn expects_response(&self) -> bool {
        self.method.is_some()
    }
}

/// The error object defined by JSON-RPC 2.0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    /// Render as the error shape MCP clients expect inside `CallToolResult`.
    pub fn to_tool_result(&self) -> Value {
        json!({
            "content": [{
                "type": "text",
                "text": format!("sandbox policy denied this call: {}", self.message),
            }],
            "isError": true,
        })
    }
}

/// MCP method names the sandbox reacts to.
pub mod methods {
    pub const INITIALIZE: &str = "initialize";
    pub const INITIALIZED: &str = "notifications/initialized";
    pub const TOOLS_LIST: &str = "tools/list";
    pub const TOOLS_CALL: &str = "tools/call";
    pub const PING: &str = "ping";
    pub const RESOURCES_LIST: &str = "resources/list";
    pub const PROMPTS_LIST: &str = "prompts/list";
}

/// Protocol revision this sandbox speaks. Forwarded verbatim to the upstream.
pub const SUPPORTED_PROTOCOL_VERSION: &str = "2025-06-18";

/// Build the `initialize` params for a proxy sitting between agent and server.
pub fn initialize_params(client_name: &str) -> Value {
    json!({
        "protocolVersion": SUPPORTED_PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "clientInfo": { "name": client_name, "version": env!("CARGO_PKG_VERSION") },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrips_through_json() {
        let msg = Message::request(json!(1), methods::TOOLS_LIST, json!({}));
        let text = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&text).unwrap();
        assert_eq!(back.id, Some(json!(1)));
        assert_eq!(back.method.as_deref(), Some(methods::TOOLS_LIST));
    }

    #[test]
    fn notifications_omit_id() {
        let msg = Message::notification(methods::INITIALIZED, json!({}));
        let text = serde_json::to_string(&msg).unwrap();
        assert!(
            !text.contains("\"id\""),
            "notification must not carry an id: {text}"
        );
    }

    #[test]
    fn failure_defaults_id_to_null() {
        let msg = Message::failure(None, RpcError::PARSE_ERROR, "bad");
        assert_eq!(msg.id, Some(Value::Null));
    }

    #[test]
    fn only_requests_expect_responses() {
        assert!(Message::request(json!(1), methods::PING, json!({})).expects_response());
        assert!(!Message::success(Some(json!(1)), json!({})).expects_response());
    }
}
