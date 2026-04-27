//! Canonical JSON-RPC 2.0 error codes per
//! [the JSON-RPC 2.0 spec §5.1](https://www.jsonrpc.org/specification#error_object)
//! plus the MCP-2026 reserved range.
//!
//! The spec reserves `-32768..=-32000` for protocol-level errors. MCP
//! itself layers application errors above `-32000`. This module exposes
//! only the standard JSON-RPC codes plus a reserved-range checker.

use serde::{Deserialize, Serialize};

/// Standard JSON-RPC 2.0 error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// `-32700` Invalid JSON was received by the server.
    ParseError,
    /// `-32600` The JSON sent is not a valid Request object.
    InvalidRequest,
    /// `-32601` The method does not exist / is not available.
    MethodNotFound,
    /// `-32602` Invalid method parameter(s).
    InvalidParams,
    /// `-32603` Internal JSON-RPC error.
    InternalError,
    /// Implementation-defined server-error in `-32099..=-32000`.
    ServerError(i32),
}

impl ErrorCode {
    pub fn code(self) -> i32 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::InternalError => -32603,
            Self::ServerError(c) => c,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::ParseError => "Parse error",
            Self::InvalidRequest => "Invalid Request",
            Self::MethodNotFound => "Method not found",
            Self::InvalidParams => "Invalid params",
            Self::InternalError => "Internal error",
            Self::ServerError(_) => "Server error",
        }
    }
}

/// JSON-RPC 2.0 error object as sent on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcError {
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code: code.code(),
            message: code.message().to_string(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// Returns true if `code` falls inside the JSON-RPC 2.0 reserved range
/// `-32768..=-32000`. Application code MUST NOT use values inside this
/// range for its own errors.
pub fn is_reserved_code(code: i32) -> bool {
    (-32768..=-32000).contains(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_codes_match_spec() {
        assert_eq!(ErrorCode::ParseError.code(), -32700);
        assert_eq!(ErrorCode::InvalidRequest.code(), -32600);
        assert_eq!(ErrorCode::MethodNotFound.code(), -32601);
        assert_eq!(ErrorCode::InvalidParams.code(), -32602);
        assert_eq!(ErrorCode::InternalError.code(), -32603);
    }

    #[test]
    fn reserved_range_is_inclusive() {
        assert!(is_reserved_code(-32768));
        assert!(is_reserved_code(-32700));
        assert!(is_reserved_code(-32000));
        assert!(!is_reserved_code(-31999));
        assert!(!is_reserved_code(-32769));
        assert!(!is_reserved_code(0));
        assert!(!is_reserved_code(100));
    }

    #[test]
    fn server_error_is_in_implementation_range() {
        let custom = ErrorCode::ServerError(-32050);
        assert_eq!(custom.code(), -32050);
        assert!(is_reserved_code(custom.code()));
    }

    #[test]
    fn json_rpc_error_round_trips() {
        let err = JsonRpcError::new(ErrorCode::ParseError)
            .with_data(serde_json::json!({"hint": "expected '}'"}));
        let s = serde_json::to_string(&err).unwrap();
        let back: JsonRpcError = serde_json::from_str(&s).unwrap();
        assert_eq!(back.code, -32700);
        assert_eq!(back.message, "Parse error");
        assert_eq!(back.data, Some(serde_json::json!({"hint": "expected '}'"})));
    }

    #[test]
    fn json_rpc_error_omits_null_data() {
        let err = JsonRpcError::new(ErrorCode::InternalError);
        let s = serde_json::to_string(&err).unwrap();
        assert!(!s.contains("data"), "null data must be skipped: {s}");
    }
}
