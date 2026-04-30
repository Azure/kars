// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! JSON-RPC 2.0 frame types and parser.
//!
//! Strictly conforms to [the JSON-RPC 2.0 spec](https://www.jsonrpc.org/specification):
//!
//! - `jsonrpc` field MUST be exactly the string `"2.0"`.
//! - `id` MUST be a String, Number, or Null. Notifications have no `id`.
//! - Empty batches are invalid (per §6).
//!
//! Tampered or malformed input produces a structured [`ParseError`] that
//! the caller can map to a JSON-RPC `-32600`/`-32700` error response.
//! No panics, no silent acceptance of garbage.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 message id. The spec allows String, Number, or Null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    String(String),
    Number(i64),
    Null,
}

/// A JSON-RPC 2.0 request frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    pub id: Id,
}

/// A JSON-RPC 2.0 notification (request without `id`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A JSON-RPC 2.0 response frame. `result` and `error` are mutually
/// exclusive; this type uses serde's untagged tagging to keep the wire
/// shape canonical.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::mcp::error::JsonRpcError>,
    pub id: Id,
}

/// A parsed JSON-RPC 2.0 frame on the wire. Top-level can be a single
/// frame or a batch (array of one or more single frames). Empty batches
/// are rejected per spec §6.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Request(Request),
    Notification(Notification),
    Response(Response),
    Batch(Vec<Frame>),
}

/// Parse error categories — every variant maps to a defined JSON-RPC
/// error code.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// Wire bytes did not parse as JSON. Maps to JSON-RPC `-32700`.
    InvalidJson(String),
    /// `jsonrpc` field missing or not exactly `"2.0"`. Maps to `-32600`.
    InvalidProtocolVersion(String),
    /// JSON shape did not match any of (request, notification, response).
    /// Maps to `-32600`.
    InvalidShape(String),
    /// Empty batch array — explicitly forbidden by the spec. Maps to
    /// `-32600`.
    EmptyBatch,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(e) => write!(f, "invalid JSON: {e}"),
            Self::InvalidProtocolVersion(v) => {
                write!(f, "invalid jsonrpc version: {v} (expected '2.0')")
            }
            Self::InvalidShape(s) => write!(f, "invalid frame shape: {s}"),
            Self::EmptyBatch => write!(f, "empty batch (forbidden by JSON-RPC 2.0 §6)"),
        }
    }
}

impl std::error::Error for ParseError {}

const PROTOCOL_VERSION: &str = "2.0";

/// Parse a JSON-RPC 2.0 frame from raw bytes.
///
/// Returns a [`Frame`] on success, or a structured [`ParseError`] on
/// failure. The parser:
///
/// - Rejects any `jsonrpc` value other than the exact string `"2.0"`.
/// - Rejects empty batches.
/// - Accepts batches recursively (but flat-only — JSON-RPC 2.0 forbids
///   nested batches; nesting is detected via inner-frame parse).
pub fn parse_frame(bytes: &[u8]) -> Result<Frame, ParseError> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| ParseError::InvalidJson(e.to_string()))?;
    parse_frame_value(v)
}

fn parse_frame_value(v: Value) -> Result<Frame, ParseError> {
    match v {
        Value::Array(arr) => {
            if arr.is_empty() {
                return Err(ParseError::EmptyBatch);
            }
            let mut frames = Vec::with_capacity(arr.len());
            for item in arr {
                // JSON-RPC 2.0 §6: batch items must be individual frames,
                // not nested batches. We enforce this by rejecting Array
                // children.
                if item.is_array() {
                    return Err(ParseError::InvalidShape(
                        "nested batch not permitted".into(),
                    ));
                }
                frames.push(parse_frame_value(item)?);
            }
            Ok(Frame::Batch(frames))
        }
        Value::Object(obj) => {
            // Validate jsonrpc field.
            let version = obj
                .get("jsonrpc")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ParseError::InvalidProtocolVersion("<missing>".into()))?;
            if version != PROTOCOL_VERSION {
                return Err(ParseError::InvalidProtocolVersion(version.to_string()));
            }

            let has_method = obj.contains_key("method");
            let has_id = obj.contains_key("id");
            let has_result = obj.contains_key("result");
            let has_error = obj.contains_key("error");

            // Response: has id + (result XOR error), no method.
            if !has_method && has_id && (has_result ^ has_error) {
                let resp: Response = serde_json::from_value(Value::Object(obj))
                    .map_err(|e| ParseError::InvalidShape(format!("response: {e}")))?;
                return Ok(Frame::Response(resp));
            }

            // Request: has method + id.
            if has_method && has_id {
                let req: Request = serde_json::from_value(Value::Object(obj))
                    .map_err(|e| ParseError::InvalidShape(format!("request: {e}")))?;
                return Ok(Frame::Request(req));
            }

            // Notification: has method, no id.
            if has_method && !has_id {
                let notif: Notification = serde_json::from_value(Value::Object(obj))
                    .map_err(|e| ParseError::InvalidShape(format!("notification: {e}")))?;
                return Ok(Frame::Notification(notif));
            }

            Err(ParseError::InvalidShape(
                "object is neither request, notification, nor response".into(),
            ))
        }
        _ => Err(ParseError::InvalidShape(
            "top-level must be object or array".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_request() {
        let raw = br#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#;
        let frame = parse_frame(raw).unwrap();
        match frame {
            Frame::Request(r) => {
                assert_eq!(r.method, "tools/list");
                assert_eq!(r.id, Id::Number(1));
            }
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn parse_valid_notification() {
        let raw = br#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#;
        let frame = parse_frame(raw).unwrap();
        assert!(matches!(frame, Frame::Notification(_)));
    }

    #[test]
    fn parse_valid_response_result() {
        let raw = br#"{"jsonrpc":"2.0","result":{"ok":true},"id":1}"#;
        let frame = parse_frame(raw).unwrap();
        assert!(matches!(frame, Frame::Response(_)));
    }

    #[test]
    fn parse_valid_response_error() {
        let raw =
            br#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":7}"#;
        let frame = parse_frame(raw).unwrap();
        match frame {
            Frame::Response(r) => assert_eq!(r.error.unwrap().code, -32601),
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn rejects_wrong_version() {
        let raw = br#"{"jsonrpc":"3.0","method":"x","id":1}"#;
        let err = parse_frame(raw).unwrap_err();
        assert!(matches!(err, ParseError::InvalidProtocolVersion(_)));
    }

    #[test]
    fn rejects_jsonrpc_1_0() {
        let raw = br#"{"jsonrpc":"1.0","method":"x","id":1}"#;
        let err = parse_frame(raw).unwrap_err();
        assert!(matches!(err, ParseError::InvalidProtocolVersion(_)));
    }

    #[test]
    fn rejects_missing_version() {
        let raw = br#"{"method":"x","id":1}"#;
        let err = parse_frame(raw).unwrap_err();
        assert!(matches!(err, ParseError::InvalidProtocolVersion(_)));
    }

    #[test]
    fn rejects_invalid_json() {
        let raw = b"{not json";
        let err = parse_frame(raw).unwrap_err();
        assert!(matches!(err, ParseError::InvalidJson(_)));
    }

    #[test]
    fn rejects_empty_batch() {
        let raw = b"[]";
        let err = parse_frame(raw).unwrap_err();
        assert!(matches!(err, ParseError::EmptyBatch));
    }

    #[test]
    fn rejects_nested_batch() {
        let raw = br#"[[{"jsonrpc":"2.0","method":"x","id":1}]]"#;
        let err = parse_frame(raw).unwrap_err();
        assert!(matches!(err, ParseError::InvalidShape(s) if s.contains("nested batch")));
    }

    #[test]
    fn rejects_response_with_both_result_and_error() {
        let raw = br#"{"jsonrpc":"2.0","result":{},"error":{"code":-1,"message":"x"},"id":1}"#;
        let err = parse_frame(raw).unwrap_err();
        assert!(matches!(err, ParseError::InvalidShape(_)));
    }

    #[test]
    fn rejects_top_level_string() {
        let raw = br#""hello""#;
        let err = parse_frame(raw).unwrap_err();
        assert!(matches!(err, ParseError::InvalidShape(_)));
    }

    #[test]
    fn parses_mixed_batch() {
        let raw = br#"[
            {"jsonrpc":"2.0","method":"a","id":1},
            {"jsonrpc":"2.0","method":"b"}
        ]"#;
        let frame = parse_frame(raw).unwrap();
        match frame {
            Frame::Batch(items) => {
                assert_eq!(items.len(), 2);
                assert!(matches!(items[0], Frame::Request(_)));
                assert!(matches!(items[1], Frame::Notification(_)));
            }
            _ => panic!("expected Batch"),
        }
    }

    #[test]
    fn id_round_trips_string_number_null() {
        let cases = [
            (
                br#"{"jsonrpc":"2.0","method":"x","id":"abc"}"#.as_slice(),
                Id::String("abc".into()),
            ),
            (
                br#"{"jsonrpc":"2.0","method":"x","id":42}"#.as_slice(),
                Id::Number(42),
            ),
            (
                br#"{"jsonrpc":"2.0","method":"x","id":null}"#.as_slice(),
                Id::Null,
            ),
        ];
        for (raw, expected) in cases {
            let frame = parse_frame(raw).unwrap();
            match frame {
                Frame::Request(r) => {
                    assert_eq!(r.id, expected, "raw: {:?}", std::str::from_utf8(raw))
                }
                _ => panic!("expected Request"),
            }
        }
    }
}
