//! A2A 1.0.0 error catalogue per spec §3.3.2 (errors).
//!
//! These errors are surfaced over the JSON-RPC binding using the
//! standard JSON-RPC `code` / `message` / `data` envelope. The codes
//! below are the A2A-specific application error codes — they sit in
//! the JSON-RPC server-error reserved range (-32099..=-32000 per
//! RFC) and **MUST NOT** collide with the generic JSON-RPC codes
//! defined in [`crate::mcp::error`].

use serde::{Deserialize, Serialize};

/// A2A-specific application error codes. Spec §3.3.2.
///
/// All codes are i32 to match JSON-RPC `code` typing. We deliberately
/// keep them in the JSON-RPC server-error reserved range so generic
/// JSON-RPC clients still treat them as server errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i32", from = "i32")]
pub enum A2aErrorCode {
    TaskNotFound,
    TaskNotCancelable,
    PushNotificationNotSupported,
    UnsupportedOperation,
    ContentTypeNotSupported,
    InvalidAgentResponse,
    ExtendedAgentCardNotConfigured,
    ExtensionSupportRequired,
    VersionNotSupported,
    /// AP2 commerce extension denial — set when an inbound message
    /// carrying a `metadata.ap2` extension is rejected by the
    /// signed-mandate verifier or the policy validator (see
    /// [`crate::a2a::ap2::Ap2Denial`]). The `data.reason` field of
    /// the JSON-RPC error envelope carries the rendered denial.
    Ap2Denied,
    /// Catch-all for forward compatibility — preserves unknown codes
    /// without dropping them.
    Other(i32),
}

impl From<A2aErrorCode> for i32 {
    fn from(c: A2aErrorCode) -> i32 {
        match c {
            A2aErrorCode::TaskNotFound => -32001,
            A2aErrorCode::TaskNotCancelable => -32002,
            A2aErrorCode::PushNotificationNotSupported => -32003,
            A2aErrorCode::UnsupportedOperation => -32004,
            A2aErrorCode::ContentTypeNotSupported => -32005,
            A2aErrorCode::InvalidAgentResponse => -32006,
            A2aErrorCode::ExtendedAgentCardNotConfigured => -32007,
            A2aErrorCode::ExtensionSupportRequired => -32008,
            A2aErrorCode::VersionNotSupported => -32009,
            A2aErrorCode::Ap2Denied => -32011,
            A2aErrorCode::Other(n) => n,
        }
    }
}

impl From<i32> for A2aErrorCode {
    fn from(n: i32) -> A2aErrorCode {
        match n {
            -32001 => A2aErrorCode::TaskNotFound,
            -32002 => A2aErrorCode::TaskNotCancelable,
            -32003 => A2aErrorCode::PushNotificationNotSupported,
            -32004 => A2aErrorCode::UnsupportedOperation,
            -32005 => A2aErrorCode::ContentTypeNotSupported,
            -32006 => A2aErrorCode::InvalidAgentResponse,
            -32007 => A2aErrorCode::ExtendedAgentCardNotConfigured,
            -32008 => A2aErrorCode::ExtensionSupportRequired,
            -32009 => A2aErrorCode::VersionNotSupported,
            -32011 => A2aErrorCode::Ap2Denied,
            other => A2aErrorCode::Other(other),
        }
    }
}

impl A2aErrorCode {
    pub fn default_message(&self) -> &'static str {
        match self {
            A2aErrorCode::TaskNotFound => "Task not found",
            A2aErrorCode::TaskNotCancelable => "Task cannot be canceled",
            A2aErrorCode::PushNotificationNotSupported => "Push notifications not supported",
            A2aErrorCode::UnsupportedOperation => "Operation not supported",
            A2aErrorCode::ContentTypeNotSupported => "Content type not supported",
            A2aErrorCode::InvalidAgentResponse => "Invalid agent response",
            A2aErrorCode::ExtendedAgentCardNotConfigured => "Extended agent card not configured",
            A2aErrorCode::ExtensionSupportRequired => "Extension support required",
            A2aErrorCode::VersionNotSupported => "A2A version not supported",
            A2aErrorCode::Ap2Denied => "AP2 commerce extension denied",
            A2aErrorCode::Other(_) => "A2A error",
        }
    }
}

/// A2A error envelope serialised as JSON-RPC `error` payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct A2aError {
    pub code: A2aErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl A2aError {
    pub fn new(code: A2aErrorCode) -> Self {
        Self {
            code,
            message: code.default_message().into(),
            data: None,
        }
    }

    pub fn with_data(code: A2aErrorCode, data: serde_json::Value) -> Self {
        Self {
            code,
            message: code.default_message().into(),
            data: Some(data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_round_trip() {
        for c in [
            A2aErrorCode::TaskNotFound,
            A2aErrorCode::TaskNotCancelable,
            A2aErrorCode::PushNotificationNotSupported,
            A2aErrorCode::UnsupportedOperation,
            A2aErrorCode::ContentTypeNotSupported,
            A2aErrorCode::InvalidAgentResponse,
            A2aErrorCode::ExtendedAgentCardNotConfigured,
            A2aErrorCode::ExtensionSupportRequired,
            A2aErrorCode::VersionNotSupported,
        ] {
            let n: i32 = c.into();
            assert_eq!(A2aErrorCode::from(n), c);
        }
    }

    #[test]
    fn known_codes_in_jsonrpc_server_error_range() {
        for c in [
            A2aErrorCode::TaskNotFound,
            A2aErrorCode::VersionNotSupported,
        ] {
            let n: i32 = c.into();
            assert!((-32099..=-32000).contains(&n), "code {n} out of range");
        }
    }

    #[test]
    fn unknown_code_preserved_via_other() {
        let c: A2aErrorCode = (-32500).into();
        assert_eq!(c, A2aErrorCode::Other(-32500));
        let back: i32 = c.into();
        assert_eq!(back, -32500);
    }

    #[test]
    fn error_serialises_with_numeric_code() {
        let e = A2aError::new(A2aErrorCode::TaskNotFound);
        let s = serde_json::to_string(&e).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v.get("code").and_then(|c| c.as_i64()), Some(-32001));
        assert_eq!(
            v.get("message").and_then(|m| m.as_str()),
            Some("Task not found")
        );
        assert!(v.get("data").is_none());
    }

    #[test]
    fn error_with_data_round_trips() {
        let e = A2aError::with_data(
            A2aErrorCode::ContentTypeNotSupported,
            serde_json::json!({"requested": "image/heif"}),
        );
        let s = serde_json::to_string(&e).unwrap();
        let back: A2aError = serde_json::from_str(&s).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn unknown_error_code_round_trip_through_envelope() {
        let raw = r#"{"code":-32999,"message":"custom"}"#;
        let e: A2aError = serde_json::from_str(raw).unwrap();
        assert_eq!(e.code, A2aErrorCode::Other(-32999));
        assert_eq!(e.message, "custom");
    }

    #[test]
    fn no_collision_with_generic_jsonrpc_codes() {
        // Generic JSON-RPC codes per RFC: -32700, -32600..=-32603.
        let known_a2a: Vec<i32> = [
            A2aErrorCode::TaskNotFound,
            A2aErrorCode::TaskNotCancelable,
            A2aErrorCode::PushNotificationNotSupported,
            A2aErrorCode::UnsupportedOperation,
            A2aErrorCode::ContentTypeNotSupported,
            A2aErrorCode::InvalidAgentResponse,
            A2aErrorCode::ExtendedAgentCardNotConfigured,
            A2aErrorCode::ExtensionSupportRequired,
            A2aErrorCode::VersionNotSupported,
        ]
        .iter()
        .map(|c| (*c).into())
        .collect();
        for forbidden in [-32700, -32600, -32601, -32602, -32603] {
            assert!(
                !known_a2a.contains(&forbidden),
                "A2A code collides with JSON-RPC reserved {forbidden}"
            );
        }
    }
}
