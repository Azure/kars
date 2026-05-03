// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! A2A 1.0.0 AgentCard verifier conformance corpus.
//!
//! Mirrors the AP2 fixture corpus (`tests/ap2_conformance.rs`) for the
//! inbound card-verification path. Each scenario builds a real signed
//! card from a deterministic seed, optionally tampers with it, and
//! asserts that [`verify_inbound_card`] either accepts or rejects with
//! the expected [`CardVerifyError`] variant.
//!
//! Why programmatic instead of JSON fixtures: card verdicts depend on
//! real Ed25519 signatures, so a JSON corpus would either pin signing
//! keys (brittle) or duplicate the signing logic (drift risk). Building
//! the cards in-test from fixed seeds keeps the corpus self-checking.
//!
//! Each scenario corresponds to one rejection reason in [`CardVerifyError`]
//! plus an Allow case. The `coverage_floor` test asserts every
//! reachable variant is exercised.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{SigningKey, VerifyingKey};
use serde_json::Value;

use azureclaw_inference_router::a2a::{
    AgentCard, AgentCardConfig, AgentSkill, CardVerifierConfig, CardVerifyError, build_card,
    sign_card, verify_inbound_card,
};

/// Deterministic Ed25519 keypair from a single-byte seed for test reproducibility.
fn keypair(seed: u8) -> (SigningKey, VerifyingKey) {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    let vk = sk.verifying_key();
    (sk, vk)
}

fn base_config(name: &str, kid: &str, url: &str) -> AgentCardConfig {
    AgentCardConfig {
        name: name.into(),
        description: "test".into(),
        version: "1.0.0".into(),
        base_url: url.into(),
        kid: kid.into(),
        skills: vec![AgentSkill {
            id: "echo".into(),
            name: "echo".into(),
            description: "echo".into(),
            tags: vec!["t".into()],
            examples: None,
            input_modes: None,
            output_modes: None,
            security_requirements: None,
        }],
        provider: None,
        documentation_url: None,
        icon_url: None,
        streaming: None,
        push_notifications: None,
        default_input_modes: None,
        default_output_modes: None,
    }
}

fn build_signed(name: &str, kid: &str, url: &str, sk: &SigningKey) -> AgentCard {
    let config = base_config(name, kid, url);
    let card = build_card(&config).expect("build_card");
    sign_card(card, sk, kid).expect("sign_card")
}

fn now_secs() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

/// One scenario record. Captures everything needed to (a) describe the
/// rejection class, and (b) tag it for the coverage-floor check.
struct Scenario {
    #[allow(dead_code)] // surfaced in coverage_floor failure messages only
    name: &'static str,
    expected: Expected,
}

enum Expected {
    Allow,
    Reject(&'static str),
}

/// Run a single scenario closure. Each test in this file is its own
/// fn so failures point at the precise scenario; the `Scenario`
/// metadata feeds the coverage-floor check.
fn covered() -> &'static [Scenario] {
    &[
        Scenario {
            name: "happy_path",
            expected: Expected::Allow,
        },
        Scenario {
            name: "tampered_name",
            expected: Expected::Reject("Signature"),
        },
        Scenario {
            name: "tampered_url",
            expected: Expected::Reject("Signature"),
        },
        Scenario {
            name: "unknown_kid",
            expected: Expected::Reject("Signature"),
        },
        Scenario {
            name: "no_signatures_field",
            expected: Expected::Reject("Unsigned"),
        },
        Scenario {
            name: "empty_trust_anchors",
            expected: Expected::Reject("Signature"),
        },
        Scenario {
            name: "url_prefix_mismatch",
            expected: Expected::Reject("UrlPrefixMismatch"),
        },
        Scenario {
            name: "url_prefix_match",
            expected: Expected::Allow,
        },
        Scenario {
            name: "expired_card",
            expected: Expected::Reject("Expired"),
        },
        Scenario {
            name: "not_yet_valid_card",
            expected: Expected::Reject("NotYetValid"),
        },
        Scenario {
            name: "wrong_protocol_version",
            expected: Expected::Reject("ProtocolVersionMismatch"),
        },
        Scenario {
            name: "malformed_envelope",
            expected: Expected::Reject("Parse"),
        },
        Scenario {
            name: "empty_required_field",
            expected: Expected::Reject("EmptyRequiredField"),
        },
        Scenario {
            name: "malformed_freshness",
            expected: Expected::Reject("MalformedFreshness"),
        },
    ]
}

fn assert_reject(err: &CardVerifyError, kind: &str) {
    let actual = match err {
        CardVerifyError::Parse(_) => "Parse",
        CardVerifyError::EmptyRequiredField(_) => "EmptyRequiredField",
        CardVerifyError::ProtocolVersionMismatch { .. } => "ProtocolVersionMismatch",
        CardVerifyError::Unsigned => "Unsigned",
        CardVerifyError::Signature(_) => "Signature",
        CardVerifyError::UrlPrefixMismatch { .. } => "UrlPrefixMismatch",
        CardVerifyError::NotYetValid { .. } => "NotYetValid",
        CardVerifyError::Expired { .. } => "Expired",
        CardVerifyError::MalformedFreshness { .. } => "MalformedFreshness",
        CardVerifyError::ForbiddenOauthFlow { .. } => "ForbiddenOauthFlow",
    };
    assert_eq!(actual, kind, "expected {kind}, got {actual}: {err:?}",);
}

#[test]
fn happy_path() {
    let (sk, vk) = keypair(1);
    let card = build_signed("agent-a", "k1", "https://a.test", &sk);
    let bytes = serde_json::to_vec(&card).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let v = verify_inbound_card(&bytes, &cfg).expect("happy path verifies");
    assert_eq!(v.kid, "k1");
    assert_eq!(v.name, "agent-a");
}

#[test]
fn tampered_name() {
    let (sk, vk) = keypair(2);
    let card = build_signed("orig", "k1", "https://a.test", &sk);
    let mut json: Value = serde_json::to_value(&card).unwrap();
    json["name"] = serde_json::json!("evil-replaced");
    let bytes = serde_json::to_vec(&json).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "Signature");
}

#[test]
fn tampered_url() {
    let (sk, vk) = keypair(3);
    let card = build_signed("agent-b", "k1", "https://orig.test", &sk);
    let mut json: Value = serde_json::to_value(&card).unwrap();
    // Top-level URL field doesn't exist on AgentCard, so mutate the
    // first interface URL — that's part of the signed payload.
    json["supportedInterfaces"][0]["url"] = serde_json::json!("https://evil.test");
    let bytes = serde_json::to_vec(&json).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "Signature");
}

#[test]
fn unknown_kid() {
    let (sk, _vk) = keypair(4);
    let card = build_signed("agent-c", "k1", "https://c.test", &sk);
    let bytes = serde_json::to_vec(&card).unwrap();
    // Trust anchor under a *different* kid → kid lookup fails →
    // signature verification reports a key-resolution failure.
    let (_other_sk, other_vk) = keypair(99);
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("kX", &other_vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "Signature");
}

#[test]
fn no_signatures_field() {
    let (sk, vk) = keypair(5);
    let card = build_signed("agent-d", "k1", "https://d.test", &sk);
    let mut json: Value = serde_json::to_value(&card).unwrap();
    json.as_object_mut().unwrap().remove("signatures");
    let bytes = serde_json::to_vec(&json).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "Unsigned");
}

#[test]
fn empty_trust_anchors() {
    // Defence: zero-anchor config must reject every card.
    let (sk, _vk) = keypair(6);
    let card = build_signed("agent-e", "k1", "https://e.test", &sk);
    let bytes = serde_json::to_vec(&card).unwrap();
    let cfg = CardVerifierConfig {
        trusted_keys: HashMap::new(),
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "Signature");
}

#[test]
fn url_prefix_mismatch() {
    let (sk, vk) = keypair(7);
    let mut config = base_config("agent-f", "k1", "https://orig.test/a2a");
    config.provider = Some(azureclaw_inference_router::a2a::AgentProvider {
        url: "https://orig.test/a2a".into(),
        organization: "test-org".into(),
    });
    let unsigned = build_card(&config).unwrap();
    let signed = sign_card(unsigned, &sk, "k1").unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: Some("https://other.test"),
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "UrlPrefixMismatch");
}

#[test]
fn url_prefix_match() {
    let (sk, vk) = keypair(8);
    let mut config = base_config("agent-g", "k1", "https://example.test/a2a");
    config.provider = Some(azureclaw_inference_router::a2a::AgentProvider {
        url: "https://example.test/a2a".into(),
        organization: "test-org".into(),
    });
    let unsigned = build_card(&config).unwrap();
    let signed = sign_card(unsigned, &sk, "k1").unwrap();
    let bytes = serde_json::to_vec(&signed).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: Some("https://example.test"),
        now: now_secs(),
    };
    let v = verify_inbound_card(&bytes, &cfg).expect("prefix-matched card verifies");
    assert_eq!(v.kid, "k1");
}

#[test]
fn expired_card() {
    let (sk, vk) = keypair(9);
    let card = build_signed("agent-h", "k1", "https://h.test", &sk);
    let mut json: Value = serde_json::to_value(&card).unwrap();
    // Inject validUntil before now_secs() into the *raw* JSON so that
    // both the canonical signing payload and the peeked envelope agree.
    // The card itself doesn't carry validUntil; the verifier peeks for
    // it on the envelope. Re-sign over the modified payload.
    json["validUntil"] = serde_json::json!("2020-01-01T00:00:00Z");
    // Re-sign by stripping signatures, re-serialising, then re-running
    // sign_card. Easiest: parse back into AgentCard (validUntil ignored
    // because not a typed field), keep the typed signing input, then
    // append validUntil to the wire-form JSON before serialising.
    // Since validUntil is on the envelope (not signed), we don't need
    // to re-sign — verifier checks validUntil before signature.
    let bytes = serde_json::to_vec(&json).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "Expired");
}

#[test]
fn not_yet_valid_card() {
    let (sk, vk) = keypair(10);
    let card = build_signed("agent-i", "k1", "https://i.test", &sk);
    let mut json: Value = serde_json::to_value(&card).unwrap();
    json["validFrom"] = serde_json::json!("2099-01-01T00:00:00Z");
    let bytes = serde_json::to_vec(&json).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "NotYetValid");
}

#[test]
fn wrong_protocol_version() {
    let (sk, vk) = keypair(11);
    let card = build_signed("agent-j", "k1", "https://j.test", &sk);
    let mut json: Value = serde_json::to_value(&card).unwrap();
    json["protocolVersion"] = serde_json::json!("9.99");
    let bytes = serde_json::to_vec(&json).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "ProtocolVersionMismatch");
}

#[test]
fn malformed_envelope() {
    let bytes = b"{not valid json";
    let cfg = CardVerifierConfig {
        trusted_keys: HashMap::new(),
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(bytes, &cfg).unwrap_err();
    assert_reject(&err, "Parse");
}

#[test]
fn empty_required_field() {
    let (sk, vk) = keypair(12);
    let card = build_signed("agent-k", "k1", "https://k.test", &sk);
    let mut json: Value = serde_json::to_value(&card).unwrap();
    json["name"] = serde_json::json!("");
    let bytes = serde_json::to_vec(&json).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "EmptyRequiredField");
}

#[test]
fn malformed_freshness() {
    let (sk, vk) = keypair(13);
    let card = build_signed("agent-l", "k1", "https://l.test", &sk);
    let mut json: Value = serde_json::to_value(&card).unwrap();
    json["validUntil"] = serde_json::json!("not-a-real-rfc3339-timestamp");
    let bytes = serde_json::to_vec(&json).unwrap();
    let mut keys: HashMap<&str, &VerifyingKey> = HashMap::new();
    keys.insert("k1", &vk);
    let cfg = CardVerifierConfig {
        trusted_keys: keys,
        expected_url_prefix: None,
        now: now_secs(),
    };
    let err = verify_inbound_card(&bytes, &cfg).unwrap_err();
    assert_reject(&err, "MalformedFreshness");
}

/// Coverage floor — every `CardVerifyError` variant we ship in
/// production must appear at least once across the corpus, and we
/// must have at least one `Allow` case. Update this when adding new
/// verifier errors.
#[test]
fn coverage_floor() {
    let scenarios = covered();
    let kinds: HashSet<&str> = scenarios
        .iter()
        .filter_map(|s| match s.expected {
            Expected::Allow => None,
            Expected::Reject(k) => Some(k),
        })
        .collect();
    let allows = scenarios
        .iter()
        .filter(|s| matches!(s.expected, Expected::Allow))
        .count();

    let required: HashSet<&str> = [
        "Parse",
        "EmptyRequiredField",
        "ProtocolVersionMismatch",
        "Unsigned",
        "Signature",
        "UrlPrefixMismatch",
        "NotYetValid",
        "Expired",
        "MalformedFreshness",
    ]
    .into_iter()
    .collect();

    let missing: Vec<&&str> = required.iter().filter(|r| !kinds.contains(*r)).collect();
    assert!(
        missing.is_empty(),
        "verifier corpus is missing coverage for: {missing:?}",
    );
    assert!(allows >= 1, "corpus must include at least one Allow case");
}
