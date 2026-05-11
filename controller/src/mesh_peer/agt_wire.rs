// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! AGT relay/registry wire-protocol shapes.
//!
//! The AGT relay (`agent-governance-toolkit/.../agent-mesh/src/agentmesh/relay/app.py`)
//! speaks a different envelope from the vendored Rust relay:
//!
//! | Frame      | Shape                                                              |
//! |------------|--------------------------------------------------------------------|
//! | connect    | `{type:"connect", from:<did>, token?:<shared-secret>}`             |
//! | message    | `{type:"message", to:<did>, from:<did>, id:<uuid>, payload:<b64>}` |
//! | ack        | `{type:"ack", id:<msg-id>}`                                        |
//! | heartbeat  | `{type:"heartbeat"}`                                               |
//! | disconnect | `{type:"disconnect"}`                                              |
//! | error      | `{type:"error", detail:<str>}`                                     |
//!
//! Notes:
//! - The relay forwards `message` frames **verbatim** (it doesn't insert
//!   `from`), so the sender MUST embed its own `from` field for the
//!   recipient to know who sent the message.
//! - The relay sends NO `connected` ack after the connect handshake. The
//!   client should treat itself as connected immediately if no `error`
//!   frame arrives within a short window.
//! - The AGT registry uses `POST /v1/agents` with a totally different body
//!   shape from the vendored `/v1/registry/register` (no signature, single
//!   `public_key` field, `did` instead of `amid`).

use serde::{Deserialize, Serialize};

/// AGT relay wire frame. Tagged on `type`; covers the subset the controller
/// needs to send/receive.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgtFrame {
    /// First frame the client sends after WS upgrade.
    Connect {
        from: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        token: Option<String>,
    },
    /// Bidirectional message envelope. The relay forwards this frame
    /// verbatim to the recipient, so the sender's `from` is preserved.
    /// `payload` carries base64-encoded JSON of a `FederationMessage`.
    Message {
        to: String,
        from: String,
        id: String,
        payload: String,
    },
    /// Per-message ack — the recipient sends this after successfully
    /// processing a `Message` so the relay can purge it from the inbox.
    /// Without it, the relay redelivers on reconnect.
    Ack { id: String },
    /// Keepalive — sent every 30 s by the client. The relay updates
    /// `last_heartbeat` and does NOT reply.
    Heartbeat,
    /// Optional clean disconnect — the relay closes the socket.
    Disconnect,
    /// Server-side error response (e.g. unknown frame type, auth fail).
    Error { detail: String },
}

/// AGT registry `POST /v1/agents` body. Mirrors `RegisterAgentRequest` in
/// `agent-mesh/.../registry/app.py`.
#[derive(Debug, Serialize)]
pub struct AgtRegisterAgentRequest {
    pub did: String,
    /// Base64url-encoded Ed25519 signing public key (32 bytes).
    pub public_key: String,
    pub capabilities: Vec<String>,
    pub metadata: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_frame_serializes_with_snake_case() {
        let f = AgtFrame::Connect {
            from: "did:agentmesh:abc".into(),
            token: Some("secret".into()),
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"type\":\"connect\""));
        assert!(json.contains("\"from\":\"did:agentmesh:abc\""));
        assert!(json.contains("\"token\":\"secret\""));
    }

    #[test]
    fn connect_frame_omits_token_when_none() {
        let f = AgtFrame::Connect {
            from: "did:agentmesh:abc".into(),
            token: None,
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(!json.contains("token"));
    }

    #[test]
    fn message_frame_roundtrip() {
        let f = AgtFrame::Message {
            to: "did:agentmesh:peer".into(),
            from: "did:agentmesh:me".into(),
            id: "msg-1".into(),
            payload: "base64data".into(),
        };
        let json = serde_json::to_string(&f).unwrap();
        let decoded: AgtFrame = serde_json::from_str(&json).unwrap();
        match decoded {
            AgtFrame::Message {
                to,
                from,
                id,
                payload,
            } => {
                assert_eq!(to, "did:agentmesh:peer");
                assert_eq!(from, "did:agentmesh:me");
                assert_eq!(id, "msg-1");
                assert_eq!(payload, "base64data");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn ack_frame_roundtrip() {
        let f = AgtFrame::Ack { id: "msg-7".into() };
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(json, r#"{"type":"ack","id":"msg-7"}"#);
    }

    #[test]
    fn heartbeat_frame() {
        let f = AgtFrame::Heartbeat;
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(json, r#"{"type":"heartbeat"}"#);
    }

    #[test]
    fn error_frame_uses_detail_field() {
        let json = r#"{"type":"error","detail":"Missing 'from' field"}"#;
        let f: AgtFrame = serde_json::from_str(json).unwrap();
        match f {
            AgtFrame::Error { detail } => assert_eq!(detail, "Missing 'from' field"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn register_request_body_shape() {
        let req = AgtRegisterAgentRequest {
            did: "did:agentmesh:ctrl-123".into(),
            public_key: "abcd1234".into(),
            capabilities: vec!["offload".into(), "pairing".into()],
            metadata: std::collections::HashMap::from([(
                "display_name".into(),
                "azureclaw-controller".into(),
            )]),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["did"], "did:agentmesh:ctrl-123");
        assert_eq!(json["public_key"], "abcd1234");
        assert!(json["capabilities"].is_array());
        assert_eq!(json["metadata"]["display_name"], "azureclaw-controller");
        // Must NOT have vendored fields
        assert!(json.get("amid").is_none());
        assert!(json.get("signing_public_key").is_none());
        assert!(json.get("signature").is_none());
    }
}
