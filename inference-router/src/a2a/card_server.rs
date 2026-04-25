//! A2A 1.0.0 AgentCard server-side build pipeline — end-to-end.
//!
//! Given a declarative [`AgentCardConfig`] + [`SigningKey`], produces
//! the JSON bytes that the future GET `/.well-known/agent.json` route
//! will serve. Pure synchronous function. The route handler is a thin
//! wrapper around [`build_signed_card`]: invoke at sandbox startup,
//! cache the bytes, serve forever (or until `azureclaw push` rotates
//! the key).
//!
//! This mirrors the symmetry already established by [`crate::mcp::pipeline`]:
//! the wire-format-producing logic lives in a pure function with full
//! test coverage; the route handler is transport plumbing only.
//!
//! # Spec
//!
//! - A2A 1.0.0 §4.4 — discovery objects (the AgentCard shape).
//! - A2A 1.0.0 §4.4.7 — `AgentCardSignature`, the JWS envelope.
//! - A2A 1.0.0 §3 — `/.well-known/agent.json` discovery.
//!
//! # Total function
//!
//! All inputs that produce a malformed card are caught at build time
//! and surfaced as [`CardServerError`]. The function never panics, has
//! no I/O, and does not depend on global state.

use ed25519_dalek::SigningKey;

use super::agent_card::{
    A2A_PROTOCOL_VERSION, AgentCapabilities, AgentCard, AgentInterface, AgentProvider, AgentSkill,
    KnownProtocolBinding, ProtocolBinding,
};
use super::card_signing::{CardSignError, sign_card};

/// Errors raised by [`build_signed_card`].
#[derive(Debug, thiserror::Error)]
pub enum CardServerError {
    /// Caller supplied no skills. A2A 1.0.0 §4.4.5 implies a skill
    /// list — an empty card is technically valid wire-shape but
    /// useless; we refuse it here so misconfigurations surface at
    /// sandbox startup, not at the first inbound caller.
    #[error("agent card must declare at least one skill")]
    NoSkills,

    /// Caller supplied no interfaces. Without an interface the card
    /// cannot be reached, which means no peer can ever use it.
    #[error("agent card must declare at least one interface")]
    NoInterfaces,

    /// `name` was empty. The card's `name` is the registry-side
    /// identifier; an empty value collides with every other empty-name
    /// card in the directory.
    #[error("agent card name is required")]
    EmptyName,

    /// JWS signing failed.
    #[error("sign card: {0}")]
    Sign(#[from] CardSignError),

    /// Final serialisation failed. Should not happen for owned values
    /// but surfaced rather than panicked.
    #[error("serialise signed card: {0}")]
    Serialise(String),
}

/// Declarative inputs for the AgentCard a sandbox should publish.
///
/// All fields are owned strings/values so the config can come from a
/// `ClawSandbox.spec.a2a.*` projection, an env var, a file, or any
/// other source without lifetime gymnastics.
#[derive(Debug, Clone)]
pub struct AgentCardConfig {
    pub name: String,
    pub description: String,
    pub version: String,
    pub base_url: String,
    /// Signing key id ("kid") embedded in the JWS protected header.
    pub kid: String,
    pub skills: Vec<AgentSkill>,
    /// Optional provider (organisation + URL) per spec §4.4.2.
    pub provider: Option<AgentProvider>,
    /// `documentationUrl` per spec §4.4.1.
    pub documentation_url: Option<String>,
    /// `iconUrl` per spec §4.4.1.
    pub icon_url: Option<String>,
    /// Default streaming capability declaration. `None` omits.
    pub streaming: Option<bool>,
    /// Default `pushNotifications` capability declaration. `None` omits.
    pub push_notifications: Option<bool>,
    /// Override default input modes. Defaults to `["text/plain"]`.
    pub default_input_modes: Option<Vec<String>>,
    /// Override default output modes. Defaults to `["text/plain"]`.
    pub default_output_modes: Option<Vec<String>>,
}

/// Build, sign, and serialise the AgentCard for serving at
/// `/.well-known/agent.json`.
///
/// The single JSON-RPC interface is built from `config.base_url`. If
/// the caller wants additional bindings (gRPC, HTTP+JSON), use
/// [`build_card`] to produce the unsigned card and then mutate
/// `supported_interfaces` before calling [`super::sign_card`].
pub fn build_signed_card(
    config: &AgentCardConfig,
    signing_key: &SigningKey,
) -> Result<Vec<u8>, CardServerError> {
    let card = build_card(config)?;
    let signed = sign_card(card, signing_key, &config.kid)?;
    serde_json::to_vec(&signed).map_err(|e| CardServerError::Serialise(e.to_string()))
}

/// Build an unsigned AgentCard from `config`. Exposed for callers that
/// want to add custom interfaces or signatures before serving.
pub fn build_card(config: &AgentCardConfig) -> Result<AgentCard, CardServerError> {
    if config.name.trim().is_empty() {
        return Err(CardServerError::EmptyName);
    }
    if config.skills.is_empty() {
        return Err(CardServerError::NoSkills);
    }

    let interfaces = vec![AgentInterface {
        url: config.base_url.clone(),
        protocol_binding: ProtocolBinding::Known(KnownProtocolBinding::JsonRpc),
        tenant: None,
        protocol_version: A2A_PROTOCOL_VERSION.to_string(),
    }];

    if interfaces.is_empty() {
        return Err(CardServerError::NoInterfaces);
    }

    let capabilities = AgentCapabilities {
        streaming: config.streaming,
        push_notifications: config.push_notifications,
        extensions: None,
        extended_agent_card: None,
    };

    Ok(AgentCard {
        name: config.name.clone(),
        description: config.description.clone(),
        supported_interfaces: interfaces,
        provider: config.provider.clone(),
        version: config.version.clone(),
        documentation_url: config.documentation_url.clone(),
        capabilities,
        security_schemes: None,
        security_requirements: None,
        default_input_modes: config
            .default_input_modes
            .clone()
            .unwrap_or_else(|| vec!["text/plain".to_string()]),
        default_output_modes: config
            .default_output_modes
            .clone()
            .unwrap_or_else(|| vec!["text/plain".to_string()]),
        skills: config.skills.clone(),
        signatures: None,
        icon_url: config.icon_url.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::card_signing::{TrustedKeys, verify_card};
    use ed25519_dalek::SigningKey;
    use rand::TryRngCore;
    use std::collections::HashMap;

    fn rand_key() -> SigningKey {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.try_fill_bytes(&mut bytes).unwrap();
        SigningKey::from_bytes(&bytes)
    }

    fn skill(id: &str) -> AgentSkill {
        AgentSkill {
            id: id.into(),
            name: id.into(),
            description: format!("desc-{id}"),
            tags: vec!["test".into()],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        }
    }

    fn cfg() -> AgentCardConfig {
        AgentCardConfig {
            name: "research-agent".into(),
            description: "Reads and answers".into(),
            version: "0.1.0".into(),
            base_url: "https://example.com/a2a/research".into(),
            kid: "tenant-1-2026-04-25".into(),
            skills: vec![skill("search"), skill("summarise")],
            provider: Some(AgentProvider {
                url: "https://example.com".into(),
                organization: "ExampleCo".into(),
            }),
            documentation_url: Some("https://example.com/docs".into()),
            icon_url: None,
            streaming: Some(true),
            push_notifications: None,
            default_input_modes: None,
            default_output_modes: None,
        }
    }

    #[test]
    fn build_card_happy_path() {
        let card = build_card(&cfg()).unwrap();
        assert_eq!(card.name, "research-agent");
        assert_eq!(card.skills.len(), 2);
        assert_eq!(card.supported_interfaces.len(), 1);
        assert_eq!(card.supported_interfaces[0].protocol_version, "1.0");
        assert_eq!(card.default_input_modes, vec!["text/plain"]);
        assert_eq!(card.default_output_modes, vec!["text/plain"]);
        assert_eq!(card.capabilities.streaming, Some(true));
        assert!(card.capabilities.push_notifications.is_none());
        assert!(card.signatures.is_none());
    }

    #[test]
    fn build_card_rejects_empty_name() {
        let mut c = cfg();
        c.name = "   ".into();
        let err = build_card(&c).unwrap_err();
        assert!(matches!(err, CardServerError::EmptyName));
    }

    #[test]
    fn build_card_rejects_no_skills() {
        let mut c = cfg();
        c.skills.clear();
        let err = build_card(&c).unwrap_err();
        assert!(matches!(err, CardServerError::NoSkills));
    }

    #[test]
    fn build_card_default_modes_overridable() {
        let mut c = cfg();
        c.default_input_modes = Some(vec!["application/json".into()]);
        c.default_output_modes = Some(vec!["application/json".into(), "text/plain".into()]);
        let card = build_card(&c).unwrap();
        assert_eq!(card.default_input_modes, vec!["application/json"]);
        assert_eq!(
            card.default_output_modes,
            vec!["application/json".to_string(), "text/plain".into()]
        );
    }

    #[test]
    fn build_signed_card_roundtrips_through_verify() {
        let key = rand_key();
        let bytes = build_signed_card(&cfg(), &key).unwrap();
        // Bytes are valid UTF-8 JSON.
        let parsed: AgentCard = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.name, "research-agent");
        let sigs = parsed.signatures.as_ref().expect("signed");
        assert_eq!(sigs.len(), 1);

        // Verifies against pinned VerifyingKey.
        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("tenant-1-2026-04-25", &vk);
        let kid = verify_card(&parsed, &trusted).unwrap();
        assert_eq!(kid, "tenant-1-2026-04-25");
    }

    #[test]
    fn build_signed_card_emits_camel_case_wire_form() {
        // A2A spec §4.4.1 — wire format is camelCase. Regression
        // guard: any change to derive macros that loses the rename
        // breaks interop with reference implementations.
        let key = rand_key();
        let bytes = build_signed_card(&cfg(), &key).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(
            s.contains("\"supportedInterfaces\""),
            "camelCase missing: {s}"
        );
        assert!(s.contains("\"defaultInputModes\""));
        assert!(s.contains("\"defaultOutputModes\""));
        assert!(s.contains("\"protocolVersion\""));
        assert!(s.contains("\"protocolBinding\""));
        // `documentationUrl` only appears when `Some`.
        assert!(s.contains("\"documentationUrl\""));
        // snake_case must NOT leak.
        assert!(!s.contains("\"supported_interfaces\""));
        assert!(!s.contains("\"default_input_modes\""));
    }

    #[test]
    fn build_signed_card_omits_unset_optionals() {
        let mut c = cfg();
        c.documentation_url = None;
        c.icon_url = None;
        c.provider = None;
        c.streaming = None;
        c.push_notifications = None;
        let key = rand_key();
        let bytes = build_signed_card(&c, &key).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(!s.contains("\"documentationUrl\""), "must omit None");
        assert!(!s.contains("\"iconUrl\""));
        assert!(!s.contains("\"provider\""));
        // Capabilities may be `{}` but must not contain the omitted fields.
        assert!(!s.contains("\"streaming\""));
        assert!(!s.contains("\"pushNotifications\""));
    }

    #[test]
    fn build_signed_card_protocol_version_pinned() {
        let key = rand_key();
        let bytes = build_signed_card(&cfg(), &key).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(
            s.contains("\"protocolVersion\":\"1.0\""),
            "A2A 1.0 pin lost"
        );
    }

    #[test]
    fn signed_card_tamper_breaks_verify() {
        let key = rand_key();
        let bytes = build_signed_card(&cfg(), &key).unwrap();
        let mut card: AgentCard = serde_json::from_slice(&bytes).unwrap();

        // Mutate description after signing — should break verification.
        card.description = "Tampered description".into();

        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("tenant-1-2026-04-25", &vk);
        let err = verify_card(&card, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn signed_card_with_wrong_kid_does_not_verify() {
        let key = rand_key();
        let bytes = build_signed_card(&cfg(), &key).unwrap();
        let card: AgentCard = serde_json::from_slice(&bytes).unwrap();

        // Trust a different kid — should not match.
        let vk = key.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("other-kid", &vk);
        let err = verify_card(&card, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn signed_card_signed_with_different_key_rejected() {
        let key_a = rand_key();
        let key_b = rand_key();
        let bytes = build_signed_card(&cfg(), &key_a).unwrap();
        let card: AgentCard = serde_json::from_slice(&bytes).unwrap();

        // Trust kid `tenant-1-2026-04-25` but to key_b's verifying key.
        let vk_b = key_b.verifying_key();
        let mut trusted: TrustedKeys = HashMap::new();
        trusted.insert("tenant-1-2026-04-25", &vk_b);
        let err = verify_card(&card, &trusted).unwrap_err();
        assert!(matches!(err, CardSignError::NoTrustedSignatureValid));
    }

    #[test]
    fn build_card_skill_list_preserved() {
        let card = build_card(&cfg()).unwrap();
        let ids: Vec<&str> = card.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["search", "summarise"]);
    }

    #[test]
    fn build_card_with_provider_emits_camel_case_provider_fields() {
        let key = rand_key();
        let bytes = build_signed_card(&cfg(), &key).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("\"provider\""));
        assert!(s.contains("\"organization\""));
    }

    #[test]
    fn signed_card_signatures_are_appendable() {
        // Multi-signer: build_signed_card produces a card with one sig.
        // Calling sign_card again with another key appends a second.
        // verify_card should accept under either trust anchor.
        let key_a = rand_key();
        let key_b = rand_key();
        let bytes = build_signed_card(&cfg(), &key_a).unwrap();
        let card: AgentCard = serde_json::from_slice(&bytes).unwrap();
        let card = sign_card(card, &key_b, "co-signer-2026").unwrap();

        let vk_a = key_a.verifying_key();
        let vk_b = key_b.verifying_key();

        // Trust only A.
        let mut trust_a: TrustedKeys = HashMap::new();
        trust_a.insert("tenant-1-2026-04-25", &vk_a);
        assert_eq!(verify_card(&card, &trust_a).unwrap(), "tenant-1-2026-04-25");

        // Trust only B.
        let mut trust_b: TrustedKeys = HashMap::new();
        trust_b.insert("co-signer-2026", &vk_b);
        assert_eq!(verify_card(&card, &trust_b).unwrap(), "co-signer-2026");

        // Trust neither.
        let trust_none: TrustedKeys = HashMap::new();
        assert!(matches!(
            verify_card(&card, &trust_none),
            Err(CardSignError::NoTrustedSignatureValid | CardSignError::NoSignatures)
        ));
    }
}
