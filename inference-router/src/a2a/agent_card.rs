//! A2A 1.0.0 AgentCard data model per spec §4.4.
//!
//! Spec: <https://a2a-protocol.org/v1.0.0/specification#44-agent-discovery-objects>

use serde::{Deserialize, Serialize};

/// Pinned A2A protocol version we currently target.
pub const A2A_PROTOCOL_VERSION: &str = "1.0";

/// Self-describing manifest for an agent. Served at
/// `/.well-known/agent.json`. See spec §4.4.1.
///
/// Wire format is camelCase per A2A 1.0.0 spec §4.4.1. Rust field
/// names use snake_case; serde renames on the boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub supported_interfaces: Vec<AgentInterface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    pub capabilities: AgentCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_schemes: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_requirements: Option<serde_json::Value>,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
    pub skills: Vec<AgentSkill>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signatures: Option<Vec<AgentCardSignature>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

/// Service provider of an agent. Spec §4.4.2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProvider {
    pub url: String,
    pub organization: String,
}

/// Optional capabilities supported by an agent. Spec §4.4.3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<AgentExtension>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extended_agent_card: Option<bool>,
}

/// Protocol extension declaration. Spec §4.4.4.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentExtension {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Distinct capability or function the agent can perform. Spec §4.4.5.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_modes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_modes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_requirements: Option<serde_json::Value>,
}

/// Combination of target URL, transport binding, and protocol version
/// the agent is reachable via. Spec §4.4.6.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    pub url: String,
    pub protocol_binding: ProtocolBinding,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub protocol_version: String,
}

/// Officially recognised protocol bindings. The spec leaves the field as
/// "open form string" — we treat unknown values as `Other(...)` so we
/// never reject inbound cards with novel bindings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ProtocolBinding {
    Known(KnownProtocolBinding),
    Other(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[allow(clippy::upper_case_acronyms)]
pub enum KnownProtocolBinding {
    #[serde(rename = "JSONRPC")]
    JsonRpc,
    #[serde(rename = "GRPC")]
    Grpc,
    #[serde(rename = "HTTP+JSON")]
    HttpJson,
}

/// JWS signature of an AgentCard in flat JSON serialisation. Spec §4.4.7.
///
/// Conforms to RFC 7515 flat JSON serialisation. The full agent card
/// **without** the `signatures` field is the JWS payload — this matches
/// the spec's "self-signed manifest" semantics where the signature is
/// embedded back into the document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentCardSignature {
    pub protected: String,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_card() -> AgentCard {
        AgentCard {
            name: "test-agent".into(),
            description: "test".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://example.com/a2a".into(),
                protocol_binding: ProtocolBinding::Known(KnownProtocolBinding::JsonRpc),
                tenant: None,
                protocol_version: "1.0".into(),
            }],
            provider: None,
            version: "0.1.0".into(),
            documentation_url: None,
            capabilities: AgentCapabilities::default(),
            security_schemes: None,
            security_requirements: None,
            default_input_modes: vec!["text/plain".into()],
            default_output_modes: vec!["text/plain".into()],
            skills: vec![],
            signatures: None,
            icon_url: None,
        }
    }

    #[test]
    fn protocol_version_pinned() {
        assert_eq!(A2A_PROTOCOL_VERSION, "1.0");
    }

    #[test]
    fn minimal_card_round_trips() {
        let card = minimal_card();
        let s = serde_json::to_string(&card).unwrap();
        let back: AgentCard = serde_json::from_str(&s).unwrap();
        assert_eq!(back, card);
    }

    #[test]
    fn optional_fields_omitted_when_absent() {
        let card = minimal_card();
        let s = serde_json::to_string(&card).unwrap();
        // Per spec semantics: optional fields MUST NOT appear when
        // unset.
        assert!(!s.contains("provider"));
        assert!(!s.contains("documentationUrl"));
        assert!(!s.contains("documentation_url"));
        assert!(!s.contains("iconUrl"));
        assert!(!s.contains("icon_url"));
        assert!(!s.contains("signatures"));
    }

    #[test]
    fn required_fields_always_present() {
        let card = minimal_card();
        let s = serde_json::to_string(&card).unwrap();
        for required in ["name", "description", "version", "capabilities", "skills"] {
            assert!(s.contains(required), "missing required field: {required}");
        }
    }

    #[test]
    fn known_protocol_binding_serialises_as_string() {
        let s =
            serde_json::to_string(&ProtocolBinding::Known(KnownProtocolBinding::JsonRpc)).unwrap();
        assert_eq!(s, "\"JSONRPC\"");
        let s = serde_json::to_string(&ProtocolBinding::Known(KnownProtocolBinding::Grpc)).unwrap();
        assert_eq!(s, "\"GRPC\"");
        let s =
            serde_json::to_string(&ProtocolBinding::Known(KnownProtocolBinding::HttpJson)).unwrap();
        assert_eq!(s, "\"HTTP+JSON\"");
    }

    #[test]
    fn unknown_protocol_binding_round_trips_as_other() {
        let raw = "\"WebSocket+CBOR\"";
        let pb: ProtocolBinding = serde_json::from_str(raw).unwrap();
        match pb {
            ProtocolBinding::Other(s) => assert_eq!(s, "WebSocket+CBOR"),
            ProtocolBinding::Known(_) => panic!("should be Other"),
        }
    }

    #[test]
    fn signature_serialises_jws_flat_json_shape() {
        let sig = AgentCardSignature {
            protected: "eyJhbGciOiJFZERTQSJ9".into(),
            signature: "AAAA".into(),
            header: Some(serde_json::json!({"kid": "agent-key-1"})),
        };
        let s = serde_json::to_string(&sig).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(v.get("protected").is_some());
        assert!(v.get("signature").is_some());
        assert!(v.get("header").is_some());
    }

    #[test]
    fn signature_omits_unprotected_header_when_absent() {
        let sig = AgentCardSignature {
            protected: "eyJhbGciOiJFZERTQSJ9".into(),
            signature: "AAAA".into(),
            header: None,
        };
        let s = serde_json::to_string(&sig).unwrap();
        assert!(!s.contains("\"header\""));
    }

    #[test]
    fn agent_card_with_provider_and_signatures() {
        let mut card = minimal_card();
        card.provider = Some(AgentProvider {
            url: "https://example.com".into(),
            organization: "Example Co".into(),
        });
        card.signatures = Some(vec![AgentCardSignature {
            protected: "eyJhbGciOiJFZERTQSJ9".into(),
            signature: "AAAA".into(),
            header: None,
        }]);
        let s = serde_json::to_string(&card).unwrap();
        let back: AgentCard = serde_json::from_str(&s).unwrap();
        assert_eq!(back.provider.unwrap().organization, "Example Co");
        assert_eq!(back.signatures.unwrap().len(), 1);
    }

    #[test]
    fn agent_skill_round_trip() {
        let skill = AgentSkill {
            id: "search".into(),
            name: "Web Search".into(),
            description: "Search the web".into(),
            tags: vec!["search".into(), "web".into()],
            examples: Some(vec!["search for cats".into()]),
            input_modes: None,
            output_modes: Some(vec!["application/json".into()]),
            security_requirements: None,
        };
        let s = serde_json::to_string(&skill).unwrap();
        let back: AgentSkill = serde_json::from_str(&s).unwrap();
        assert_eq!(back, skill);
    }

    #[test]
    fn agent_capabilities_default_omits_all_fields() {
        let caps = AgentCapabilities::default();
        let s = serde_json::to_string(&caps).unwrap();
        assert_eq!(s, "{}");
    }

    #[test]
    fn agent_extension_with_required_flag() {
        let ext = AgentExtension {
            uri: Some("https://example.com/ext/v1".into()),
            description: Some("custom extension".into()),
            required: Some(true),
            params: Some(serde_json::json!({"max_size": 1024})),
        };
        let s = serde_json::to_string(&ext).unwrap();
        let back: AgentExtension = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ext);
    }

    #[test]
    fn agent_interface_with_tenant() {
        let iface = AgentInterface {
            url: "https://api.example.com/a2a/v1".into(),
            protocol_binding: ProtocolBinding::Known(KnownProtocolBinding::HttpJson),
            tenant: Some("tenant-123".into()),
            protocol_version: "1.0".into(),
        };
        let s = serde_json::to_string(&iface).unwrap();
        let back: AgentInterface = serde_json::from_str(&s).unwrap();
        assert_eq!(back, iface);
    }
}
