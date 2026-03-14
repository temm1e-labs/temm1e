//! JSON-RPC 2.0 types for MCP communication.
//!
//! MCP uses JSON-RPC 2.0 over stdio (newline-delimited) or HTTP.
//! This module defines the wire types — requests, responses, notifications, errors.

use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 request (has an `id`, expects a response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 notification (no `id`, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Incoming message from an MCP server — either a response or a notification.
/// Used by the stdio reader to dispatch incoming lines.
#[derive(Debug, Clone)]
pub enum IncomingMessage {
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        }
    }
}

impl JsonRpcNotification {
    pub fn new(method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
        }
    }
}

impl JsonRpcResponse {
    /// Check if this response is an error.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    /// Extract the result value, or return the error as a Temm1eError.
    pub fn into_result(self) -> Result<serde_json::Value, temm1e_core::types::error::Temm1eError> {
        if let Some(err) = self.error {
            Err(temm1e_core::types::error::Temm1eError::Tool(format!(
                "MCP error {}: {}",
                err.code, err.message
            )))
        } else {
            Ok(self.result.unwrap_or(serde_json::Value::Null))
        }
    }
}

/// Try to parse a line as either a JSON-RPC response or notification.
/// Returns None for unparseable lines (logged by caller).
pub fn parse_incoming(line: &str) -> Option<IncomingMessage> {
    // Try as a generic JSON value first
    let value: serde_json::Value = serde_json::from_str(line).ok()?;

    // If it has an "id" field, it's a response
    if value.get("id").is_some() {
        let resp: JsonRpcResponse = serde_json::from_value(value).ok()?;
        Some(IncomingMessage::Response(resp))
    } else if value.get("method").is_some() {
        // No id + has method = notification
        let notif: JsonRpcNotification = serde_json::from_value(value).ok()?;
        Some(IncomingMessage::Notification(notif))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_request() {
        let req = JsonRpcRequest::new(1, "initialize", Some(serde_json::json!({"key": "value"})));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"initialize\""));
    }

    #[test]
    fn serialize_request_without_params() {
        let req = JsonRpcRequest::new(2, "tools/list", None);
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("params"));
    }

    #[test]
    fn serialize_notification() {
        let notif = JsonRpcNotification::new("notifications/initialized", None);
        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("\"method\":\"notifications/initialized\""));
        assert!(!json.contains("\"id\""));
    }

    #[test]
    fn deserialize_success_response() {
        let json = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
        assert!(!resp.is_error());
    }

    #[test]
    fn deserialize_error_response() {
        let json =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json).unwrap();
        assert!(resp.is_error());
        let err = resp.into_result().unwrap_err();
        assert!(err.to_string().contains("Method not found"));
    }

    #[test]
    fn parse_incoming_response() {
        let line = r#"{"jsonrpc":"2.0","id":5,"result":{}}"#;
        match parse_incoming(line) {
            Some(IncomingMessage::Response(r)) => assert_eq!(r.id, Some(5)),
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn parse_incoming_notification() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;
        match parse_incoming(line) {
            Some(IncomingMessage::Notification(n)) => {
                assert_eq!(n.method, "notifications/tools/list_changed")
            }
            _ => panic!("Expected Notification"),
        }
    }

    #[test]
    fn parse_incoming_garbage() {
        assert!(parse_incoming("not json").is_none());
        assert!(parse_incoming("").is_none());
        assert!(parse_incoming("{}").is_none()); // no id, no method
    }

    #[test]
    fn response_into_result_ok() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            result: Some(serde_json::json!({"data": 42})),
            error: None,
        };
        let val = resp.into_result().unwrap();
        assert_eq!(val["data"], 42);
    }

    #[test]
    fn response_into_result_null() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(1),
            result: None,
            error: None,
        };
        let val = resp.into_result().unwrap();
        assert!(val.is_null());
    }
}
