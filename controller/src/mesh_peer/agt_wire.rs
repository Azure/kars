// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! AGT relay/registry wire-protocol shapes.
//!
//! The AGT relay (`agent-governance-toolkit/.../agent-mesh/src/agentmesh/relay/app.py`)
//! speaks this JSON envelope:
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
//! - The AGT registry uses `POST /v1/agents` with a single `public_key`
//!   field and `did` as the primary identifier.

use serde::{Deserialize, Serialize};

/// AGT relay wire frame. Tagged on `type`; covers the subset the controller
/// needs to send/receive.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgtFrame {
    /// First frame the client sends after WS upgrade.
    ///
    /// Since AGT main 2026-05-28 (server PR #2632) the relay enforces
    /// proof-of-possession on this frame: `public_key + timestamp +
    /// signature(timestamp)` are required, the DID must equal
    /// `did:mesh:<sha256(public_key)[:32]>`, and the legacy
    /// shared-secret `token` field is no longer accepted by the POP
    /// gate (Entra-mode `token` is a separate field validated on a
    /// different code path). Pre-2632 relays ignore the extra fields,
    /// so this shape is back-compat-safe both directions.
    Connect {
        from: String,
        /// Standard-base64 (NOT base64url) Ed25519 public key — the
        /// relay decodes with `base64.b64decode` while the registry
        /// uses `urlsafe_b64decode`. Real upstream inconsistency in
        /// `agent-mesh/.../relay/app.py:106`.
        #[serde(skip_serializing_if = "Option::is_none")]
        public_key: Option<String>,
        /// ISO-8601 UTC timestamp; the relay rejects outside a ±5 min
        /// replay window.
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<String>,
        /// Standard-base64 Ed25519 signature over the **timestamp
        /// string**, NOT over public_key||timestamp (the registry
        /// signs the latter — distinct domain separation in the
        /// upstream code).
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        /// Entra-signed JWT (AKS opt-in only). When the relay is
        /// configured with `AGENTMESH_ENTRA_AUDIENCE`, this field is
        /// required and validated against JWKS. The kars sandbox
        /// plugin's `wsFactory` injects it after the SDK builds the
        /// frame; the controller's Workload Identity path is wired
        /// in `connect_and_listen`.
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
/// `agent-mesh/.../registry/app.py` AFTER server PR #2533 (2026-05-23) —
/// the `did` field was removed (server derives it from the public key as
/// `did:mesh:<sha256(public_key)[:32]>`) and `proof + proof_timestamp`
/// became required.
///
/// Encoding gotcha: this endpoint uses `base64.urlsafe_b64decode` on the
/// server (registry/app.py:187), distinct from the relay's stdlib
/// `b64decode` on the connect-frame public_key. So `public_key`, `proof`
/// in this body must be base64**url**-encoded.
#[derive(Debug, Serialize)]
pub struct AgtRegisterAgentRequest {
    /// Base64url-encoded Ed25519 signing public key (32 bytes).
    pub public_key: String,
    /// Base64url-encoded Ed25519 signature over
    /// `public_key_b64_str || proof_timestamp_str` (the strings as
    /// transmitted, not the raw key bytes).
    pub proof: String,
    /// ISO-8601 UTC timestamp signed in the proof. ±5 min replay window.
    pub proof_timestamp: String,
    pub capabilities: Vec<String>,
    pub metadata: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_frame_serializes_with_snake_case() {
        let f = AgtFrame::Connect {
            from: "did:mesh:abc".into(),
            public_key: Some("AAAA".into()),
            timestamp: Some("2026-06-02T10:00:00.000Z".into()),
            signature: Some("BBBB".into()),
            token: Some("secret".into()),
        };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"type\":\"connect\""));
        assert!(json.contains("\"from\":\"did:mesh:abc\""));
        assert!(json.contains("\"public_key\":\"AAAA\""));
        assert!(json.contains("\"timestamp\":\"2026-06-02T10:00:00.000Z\""));
        assert!(json.contains("\"signature\":\"BBBB\""));
        assert!(json.contains("\"token\":\"secret\""));
    }

    #[test]
    fn connect_frame_omits_optional_fields_when_none() {
        let f = AgtFrame::Connect {
            from: "did:mesh:abc".into(),
            public_key: None,
            timestamp: None,
            signature: None,
            token: None,
        };
        let json = serde_json::to_string(&f).unwrap();
        // Each optional field uses skip_serializing_if = Option::is_none;
        // with all None the connect frame should be {type, from} only.
        assert!(!json.contains("public_key"));
        assert!(!json.contains("timestamp"));
        assert!(!json.contains("signature"));
        assert!(!json.contains("token"));
    }

    #[test]
    fn message_frame_roundtrip() {
        let f = AgtFrame::Message {
            to: "did:mesh:peer".into(),
            from: "did:mesh:me".into(),
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
                assert_eq!(to, "did:mesh:peer");
                assert_eq!(from, "did:mesh:me");
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
    fn register_request_body_shape_is_pop_aware() {
        // Server PR #2533 removed `did` from RegisterAgentRequest and
        // added `proof + proof_timestamp` as required fields. The DID
        // is derived server-side from sha256(public_key).
        let req = AgtRegisterAgentRequest {
            public_key: "abcd1234".into(),
            proof: "sig-base64url".into(),
            proof_timestamp: "2026-06-02T10:00:00.000Z".into(),
            capabilities: vec!["offload".into(), "pairing".into()],
            metadata: std::collections::HashMap::from([(
                "display_name".into(),
                "kars-controller".into(),
            )]),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["public_key"], "abcd1234");
        assert_eq!(json["proof"], "sig-base64url");
        assert_eq!(json["proof_timestamp"], "2026-06-02T10:00:00.000Z");
        assert!(json["capabilities"].is_array());
        assert_eq!(json["metadata"]["display_name"], "kars-controller");
        // Must NOT carry the legacy client-supplied DID — the registry
        // ignored it in v4.0.0 and rejects extras in pre-release builds
        // that may flip Pydantic to `extra="forbid"`.
        assert!(json.get("did").is_none());
        assert!(json.get("amid").is_none());
    }
}
