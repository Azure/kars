// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! End-to-end integration: trust-store hot-reload feeds card_verifier.
//!
//! This corpus stitches together the three pieces that previously lived
//! independently:
//!
//! 1. [`A2aAgentSpec`] → [`Vec<TrustAnchor>`] via
//!    `agent_projection::project_anchors` (PR 30).
//! 2. [`Vec<TrustAnchor>`] → [`TrustStoreSnapshot`] via
//!    `TrustStoreBuilder::add` + `build` (PR 24).
//! 3. [`TrustStoreSnapshot`] → `HashMap<&str, &VerifyingKey>` via
//!    `TrustStoreSnapshot::as_verifier_keys` (PR 24).
//! 4. The map → `CardVerifierConfig` consumed by
//!    `verify_inbound_card` (PR 22).
//!
//! The integration scenarios assert:
//!
//! - **Pre-install:** a card signed by `kid-new` fails when the store
//!   only contains `kid-old`.
//! - **Hot reload:** after `replace_snapshot` installs `kid-new`, the
//!   same card now verifies — without a router restart.
//! - **Reverse:** a card signed by `kid-old` continues to verify under
//!   the new snapshot if `kid-old` is still present, and fails when it
//!   is removed.
//! - **Generation monotonicity:** generation counter advances per
//!   replace.
//! - **Expiry filter:** an anchor with `not_after <= now` is filtered
//!   out by `as_verifier_keys` and the card no longer verifies.
//!
//! Why this lives in `tests/` rather than as a unit test: it exercises
//! four modules at once (`a2a::agent_projection`, `a2a::trust_store`,
//! `a2a::card_signing`, `a2a::card_verifier`). A unit test in any one
//! of them would only see one boundary; the integration test catches
//! drift across all four together.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{SigningKey, VerifyingKey};

use azureclaw_inference_router::a2a::{
    A2aAgentSigningKeySpec, A2aAgentSpec, AgentCard, AgentCardConfig, AgentSkill,
    CardVerifierConfig, TrustStore, TrustStoreBuilder, build_card, project_anchors, sign_card,
    verify_inbound_card,
};

const TEST_NAME: &str = "agent-int";
const TEST_URL: &str = "https://int.example/agent";

fn keypair(seed: u8) -> (SigningKey, VerifyingKey) {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    let vk = sk.verifying_key();
    (sk, vk)
}

fn fixed_now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000)
}

fn now_secs() -> i64 {
    1_700_000_000
}

fn agent_card_with_kid(kid: &str, sk: &SigningKey) -> AgentCard {
    let cfg = AgentCardConfig {
        name: TEST_NAME.into(),
        description: "integration".into(),
        version: "1.0.0".into(),
        base_url: TEST_URL.into(),
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
    };
    let unsigned = build_card(&cfg).expect("build_card");
    sign_card(unsigned, sk, kid).expect("sign_card")
}

fn b64url_pubkey(vk: &VerifyingKey) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(vk.as_bytes())
}

/// Build an `A2aAgentSpec` carrying one or more signing keys.
fn agent_spec_with_keys(keys: &[(&str, &VerifyingKey, Option<i64>)]) -> A2aAgentSpec {
    A2aAgentSpec {
        namespace: "test-ns".into(),
        name: "test-agent".into(),
        signing_keys: keys
            .iter()
            .map(|(kid, vk, exp)| A2aAgentSigningKeySpec {
                kid: (*kid).to_string(),
                alg: "EdDSA".to_string(),
                public_key_b64u: b64url_pubkey(vk),
                not_after: *exp,
            })
            .collect(),
    }
}

fn project_into_snapshot(
    spec: &A2aAgentSpec,
    generation: u64,
) -> azureclaw_inference_router::a2a::TrustStoreSnapshot {
    let anchors = project_anchors(spec, "test/agent").expect("project_anchors");
    let mut builder = TrustStoreBuilder::new().generation(generation);
    for a in anchors {
        builder.add(a).expect("add anchor");
    }
    builder.build()
}

#[test]
fn pre_reload_card_signed_by_unknown_kid_is_rejected() {
    let store = TrustStore::new();
    let (_sk_new, vk_new) = keypair(20);
    let (sk_other, _) = keypair(21);

    // Install only kid-old; sign a card with the new key.
    let (_sk_old, vk_old) = keypair(22);
    let _ = vk_new;
    let spec = agent_spec_with_keys(&[("kid-old", &vk_old, None)]);
    store.replace_snapshot(project_into_snapshot(&spec, 1));
    let snap = store.snapshot();

    let card = agent_card_with_kid("kid-new", &sk_other);
    let raw = serde_json::to_vec(&card).unwrap();
    let cfg = CardVerifierConfig {
        trusted_keys: snap.as_verifier_keys(now_secs()),
        expected_url_prefix: None,
        now: fixed_now(),
    };
    let res = verify_inbound_card(&raw, &cfg);
    assert!(
        res.is_err(),
        "card signed by kid-new must fail before reload"
    );
}

#[test]
fn hot_reload_makes_new_kid_verify_within_one_replace() {
    let store = TrustStore::new();

    // Initial snapshot: only kid-old.
    let (_sk_old, vk_old) = keypair(30);
    let initial = agent_spec_with_keys(&[("kid-old", &vk_old, None)]);
    store.replace_snapshot(project_into_snapshot(&initial, 1));
    assert_eq!(store.snapshot().generation(), 1);

    // Card is signed by kid-new (not yet trusted).
    let (sk_new, vk_new) = keypair(31);
    let card = agent_card_with_kid("kid-new", &sk_new);
    let raw = serde_json::to_vec(&card).unwrap();

    // Hot-reload: install kid-new alongside kid-old.
    let updated = agent_spec_with_keys(&[("kid-old", &vk_old, None), ("kid-new", &vk_new, None)]);
    store.replace_snapshot(project_into_snapshot(&updated, 2));
    let snap = store.snapshot();
    assert_eq!(snap.generation(), 2, "generation advances on replace");

    let cfg = CardVerifierConfig {
        trusted_keys: snap.as_verifier_keys(now_secs()),
        expected_url_prefix: None,
        now: fixed_now(),
    };
    verify_inbound_card(&raw, &cfg)
        .expect("kid-new card verifies after hot reload, no router restart needed");
}

#[test]
fn snapshot_replace_can_revoke_previously_trusted_kid() {
    let store = TrustStore::new();

    // Trust both old + new.
    let (sk_old, vk_old) = keypair(40);
    let (_sk_new, vk_new) = keypair(41);
    let initial = agent_spec_with_keys(&[("kid-old", &vk_old, None), ("kid-new", &vk_new, None)]);
    store.replace_snapshot(project_into_snapshot(&initial, 1));

    let card = agent_card_with_kid("kid-old", &sk_old);
    let raw = serde_json::to_vec(&card).unwrap();
    {
        let snap = store.snapshot();
        let cfg = CardVerifierConfig {
            trusted_keys: snap.as_verifier_keys(now_secs()),
            expected_url_prefix: None,
            now: fixed_now(),
        };
        verify_inbound_card(&raw, &cfg).expect("verifies under initial snapshot");
    }

    // Replace snapshot with only kid-new — this revokes kid-old.
    let revoked = agent_spec_with_keys(&[("kid-new", &vk_new, None)]);
    store.replace_snapshot(project_into_snapshot(&revoked, 2));
    let snap = store.snapshot();
    let cfg = CardVerifierConfig {
        trusted_keys: snap.as_verifier_keys(now_secs()),
        expected_url_prefix: None,
        now: fixed_now(),
    };
    let res = verify_inbound_card(&raw, &cfg);
    assert!(res.is_err(), "kid-old card must fail after revocation");
}

#[test]
fn anchor_expiry_filtered_by_as_verifier_keys() {
    let store = TrustStore::new();
    let (sk, vk) = keypair(50);

    // Anchor expires at now_secs() exactly. as_verifier_keys filters
    // strictly: `now < not_after`, so an anchor with `not_after == now`
    // is *not* present in the projected map.
    let spec = agent_spec_with_keys(&[("kid-x", &vk, Some(now_secs()))]);
    store.replace_snapshot(project_into_snapshot(&spec, 1));
    let snap = store.snapshot();

    let card = agent_card_with_kid("kid-x", &sk);
    let raw = serde_json::to_vec(&card).unwrap();
    let cfg = CardVerifierConfig {
        trusted_keys: snap.as_verifier_keys(now_secs()),
        expected_url_prefix: None,
        now: fixed_now(),
    };
    let res = verify_inbound_card(&raw, &cfg);
    assert!(res.is_err(), "expired anchor must not verify");
}

#[test]
fn anchor_with_future_expiry_still_verifies() {
    let store = TrustStore::new();
    let (sk, vk) = keypair(60);
    let spec = agent_spec_with_keys(&[("kid-y", &vk, Some(now_secs() + 3600))]);
    store.replace_snapshot(project_into_snapshot(&spec, 1));
    let snap = store.snapshot();

    let card = agent_card_with_kid("kid-y", &sk);
    let raw = serde_json::to_vec(&card).unwrap();
    let cfg = CardVerifierConfig {
        trusted_keys: snap.as_verifier_keys(now_secs()),
        expected_url_prefix: None,
        now: fixed_now(),
    };
    verify_inbound_card(&raw, &cfg).expect("future expiry verifies");
}

#[test]
fn empty_snapshot_rejects_every_card() {
    let store = TrustStore::new();
    // Default empty store, generation 0, len 0.
    let snap = store.snapshot();
    assert!(snap.is_empty());

    let (sk, _vk) = keypair(70);
    let card = agent_card_with_kid("kid-z", &sk);
    let raw = serde_json::to_vec(&card).unwrap();

    let cfg = CardVerifierConfig {
        trusted_keys: snap.as_verifier_keys(now_secs()),
        expected_url_prefix: None,
        now: fixed_now(),
    };
    let res = verify_inbound_card(&raw, &cfg);
    assert!(res.is_err(), "empty trust store must reject every card");
}
