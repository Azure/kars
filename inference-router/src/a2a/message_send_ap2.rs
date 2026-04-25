//! AP2-aware `message/send` wrapper for A2A 1.0.0 JSON-RPC dispatch.
//!
//! Spec context:
//!
//! - A2A 1.0.0 §3.3.1 — `message/send` body shape (handled by
//!   [`super::jsonrpc_dispatch::handle_message_send`]).
//! - AP2 commerce extension — when an inbound A2A message carries
//!   `params.message.metadata.ap2 = {mandate, attempt}`, the message
//!   asks the receiving agent to perform a **billable transfer** on
//!   behalf of the mandate's principal. Such messages must not reach
//!   the task store unless the mandate signature is verified, the
//!   policy envelope holds, and the attempt is recorded against the
//!   mandate ledger for replay/window enforcement.
//!
//! ## What this module does
//!
//! [`handle_message_send_with_ap2`] is the production entry point
//! that the gateway calls. Order of operations:
//!
//! 1. Inspect `params.message.metadata.ap2`.
//!    - **Absent** → delegate unchanged to
//!      [`super::jsonrpc_dispatch::handle_message_send`]. AP2-free
//!      messages pay zero overhead.
//!    - **Present but malformed** → JSON-RPC `InvalidParams` error.
//! 2. Resolve the mandate-issuer trust map from the
//!    [`MandateTrustStore`] via a snapshot view.
//! 3. Call
//!    [`crate::a2a::ap2::validate_payment_attempt_signed`]. This
//!    rejects unsigned mandates, tampered mandates, mandates signed
//!    by an unknown / wrong / expired key, and policy violations
//!    (cap exceeded, counterparty not allowed, currency mismatch,
//!    replayed nonce, mandate expired, attempt timestamp out of
//!    bounds).
//! 4. On success, append the resulting [`PaymentRecord`] to the
//!    mandate ledger via
//!    [`crate::a2a::ap2::MandateLedgerMut::record`], then delegate
//!    to the underlying `handle_message_send` so the task is created
//!    in the same `submitted` state as a non-commerce message.
//! 5. On any denial, return an `A2aErrorCode::Ap2Denied` JSON-RPC
//!    error envelope. The denial variant is rendered into the
//!    error's `data.reason` field so audit consumers can attribute
//!    the rejection reason.
//!
//! Crucially, **none of the existing `message/send` semantics
//! change** for messages that don't carry the AP2 extension. The
//! parameter shape, error envelopes, and task-creation flow are
//! identical when no `metadata.ap2` is present.
//!
//! ## Failure semantics
//!
//! - **Fail-closed by default.** An empty trust store rejects every
//!   AP2-bearing message (`MandateUnauthentic` → `Ap2Denied`).
//! - **Signature first.** The signature check runs strictly before
//!   the policy/ledger checks. An unauthentic mandate cannot
//!   observe ledger state or oracle the validator's timing.
//! - **No state mutation on rejection.** The mandate ledger is only
//!   appended to after both signature *and* policy validation pass.
//!
//! ## Total-function discipline
//!
//! The wrapper preserves the underlying handler's contract: every
//! exit path returns a fully-formed
//! [`crate::mcp::jsonrpc::Response`]; no panics, no silent
//! acceptance, no `unwrap()` on caller-controlled data.

use serde_json::Value;

use crate::mcp::jsonrpc::{Request, Response};

use super::ap2::{
    Ap2Denial, IntentMandate, MandateLedgerMut, PaymentAttempt,
    validate_payment_attempt_signed,
};
use super::error::A2aErrorCode;
use super::jsonrpc_dispatch::{
    MessageSendParams, TaskIdMinter, TaskStore, handle_message_send,
};
use super::mandate_trust_store::MandateTrustStore;

/// Wire shape of `params.message.metadata.ap2`. Mirrors the AP2 spec
/// envelope: a signed mandate plus the proposed attempt that should
/// be authorised under it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ap2MessageMetadata {
    pub mandate: IntentMandate,
    pub attempt: PaymentAttempt,
}

/// Outcome of the AP2 extension extraction step.
#[derive(Debug)]
enum Extraction {
    /// No `metadata.ap2` present — handle as a vanilla A2A message.
    None,
    /// Extension present, parsed, ready for verification.
    /// `Box`ed to keep the enum size small (`Ap2MessageMetadata`
    /// embeds the full `IntentMandate` which is much larger than
    /// the `Malformed` payload).
    Some(Box<Ap2MessageMetadata>),
    /// Extension present but malformed.
    Malformed(String),
}

fn extract_ap2(metadata: Option<&Value>) -> Extraction {
    let Some(m) = metadata else {
        return Extraction::None;
    };
    let Some(ap2) = m.get("ap2") else {
        return Extraction::None;
    };
    match serde_json::from_value::<Ap2MessageMetadata>(ap2.clone()) {
        Ok(parsed) => Extraction::Some(Box::new(parsed)),
        Err(e) => Extraction::Malformed(format!("metadata.ap2: {e}")),
    }
}

/// AP2-aware `message/send` handler. See module-level docs.
///
/// `now` is the wall-clock epoch-second timestamp used both for
/// signature-anchor expiry filtering and for the AP2 policy time
/// checks. Caller is responsible for passing a consistent value
/// (the gateway uses `OffsetDateTime::now_utc().unix_timestamp()`).
pub fn handle_message_send_with_ap2(
    req: &Request,
    store: &dyn TaskStore,
    minter: &dyn TaskIdMinter,
    mandate_trust: &MandateTrustStore,
    ledger: &mut dyn MandateLedgerMut,
    now: i64,
) -> Response {
    // Cheaply parse the params so we can peek at metadata.ap2
    // without mutating the underlying flow.
    let params: MessageSendParams = match req.params.as_ref() {
        Some(v) => match serde_json::from_value(v.clone()) {
            Ok(p) => p,
            // Underlying handler will produce a faithful
            // InvalidParams error using the same parser path.
            Err(_) => return handle_message_send(req, store, minter),
        },
        None => return handle_message_send(req, store, minter),
    };

    let extracted = extract_ap2(params.metadata.as_ref());
    match extracted {
        Extraction::None => handle_message_send(req, store, minter),
        Extraction::Malformed(reason) => invalid_params_response(req, &reason),
        Extraction::Some(ext) => {
            // Project trust snapshot to the verifier-keys map and
            // run the signed-and-policy-checked validator.
            let trust_view = mandate_trust.snapshot();
            let trusted = trust_view.as_verifier_keys(now);
            match validate_payment_attempt_signed(
                &ext.mandate,
                &ext.attempt,
                ledger,
                now,
                &trusted,
            ) {
                Ok(record) => {
                    ledger.record(record);
                    handle_message_send(req, store, minter)
                }
                Err(denial) => ap2_denied_response(req, &denial),
            }
        }
    }
}

fn invalid_params_response(req: &Request, reason: &str) -> Response {
    Response {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(crate::mcp::error::JsonRpcError {
            code: crate::mcp::error::ErrorCode::InvalidParams.code(),
            message: "Invalid params".into(),
            data: Some(serde_json::json!({"reason": reason})),
        }),
        id: req.id.clone(),
    }
}

fn ap2_denied_response(req: &Request, denial: &Ap2Denial) -> Response {
    Response {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(crate::mcp::error::JsonRpcError {
            code: A2aErrorCode::Ap2Denied.into(),
            message: A2aErrorCode::Ap2Denied.default_message().into(),
            data: Some(serde_json::json!({
                "reason": denial.to_string(),
                "kind": denial_kind(denial),
            })),
        }),
        id: req.id.clone(),
    }
}

fn denial_kind(d: &Ap2Denial) -> &'static str {
    match d {
        Ap2Denial::MandateUnauthentic(_) => "mandateUnauthentic",
        Ap2Denial::MandateExpired { .. } => "mandateExpired",
        Ap2Denial::CurrencyMismatch { .. } => "currencyMismatch",
        Ap2Denial::PerTransferCapExceeded { .. } => "perTransferCapExceeded",
        Ap2Denial::DailyCapExceeded { .. } => "dailyCapExceeded",
        Ap2Denial::MonthlyCapExceeded { .. } => "monthlyCapExceeded",
        Ap2Denial::CounterpartyNotAllowed { .. } => "counterpartyNotAllowed",
        Ap2Denial::AmountZero => "amountZero",
        Ap2Denial::ReplayDetected { .. } => "replayDetected",
        Ap2Denial::AttemptInFuture { .. } => "attemptInFuture",
        Ap2Denial::AttemptTooOld { .. } => "attemptTooOld",
        Ap2Denial::MandateIdMismatch => "mandateIdMismatch",
        Ap2Denial::ArithmeticOverflow => "arithmeticOverflow",
    }
}

/// Helper exposed for tests that need to assert the denial kind
/// string mapping is exhaustive without re-deriving it.
#[cfg(test)]
pub(crate) fn denial_kind_for_test(d: &Ap2Denial) -> &'static str {
    denial_kind(d)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    use base64::Engine;
    use ed25519_dalek::SigningKey;
    use serde_json::json;

    use super::*;
    use crate::a2a::ap2::{InMemoryMandateLedger, IntentMandate, PaymentAttempt, PaymentRecord};
    use crate::a2a::mandate_signing::sign_mandate;
    use crate::a2a::mandate_trust_store::MandateTrustStoreSnapshot;
    use crate::a2a::trust_store::{AnchorSource, TrustAnchor, TrustStoreBuilder};
    use crate::mcp::jsonrpc::{Id, Request as McpReq};

    struct CounterMinter(AtomicU64);
    impl TaskIdMinter for CounterMinter {
        fn mint(&self) -> String {
            format!("task-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }

    fn baseline_mandate(mandate_id: &str, kid: &str) -> IntentMandate {
        IntentMandate {
            mandate_id: mandate_id.into(),
            principal: "did:example:alice".into(),
            currency: "USD".into(),
            daily_cap: 10_000,
            monthly_cap: 100_000,
            per_transfer_cap: 500,
            counterparty_allowlist: {
                let mut s = BTreeSet::new();
                s.insert("did:example:bob".into());
                s
            },
            exp: 2_000_000_000,
            // Will be overwritten by sign_mandate; placeholder kid only
            // used to seed `signature` header in unsigned tests.
            signature: format!("kid={kid}"),
        }
    }

    fn baseline_attempt(mandate_id: &str, nonce: &str) -> PaymentAttempt {
        PaymentAttempt {
            mandate_id: mandate_id.into(),
            counterparty: "did:example:bob".into(),
            amount: 100,
            currency: "USD".into(),
            transfer_nonce: nonce.into(),
            timestamp: 1_700_000_000,
        }
    }

    fn sign(mandate: IntentMandate, sk: &SigningKey, kid: &str) -> IntentMandate {
        sign_mandate(&mandate, sk, kid).expect("signed mandate")
    }

    fn make_trust_store(kid: &str, sk: &SigningKey) -> MandateTrustStore {
        let mut b = TrustStoreBuilder::new().generation(1);
        b.add(TrustAnchor {
            kid: kid.into(),
            key: sk.verifying_key(),
            not_after: None,
            source: AnchorSource::from("mandate-issuer-cr/test"),
        })
        .unwrap();
        let store = MandateTrustStore::new();
        store.replace_snapshot(MandateTrustStoreSnapshot::from_inner(b.build()));
        store
    }

    fn req_with_metadata(metadata: Option<Value>) -> McpReq {
        McpReq {
            jsonrpc: "2.0".into(),
            method: "message/send".into(),
            params: Some(json!({
                "message": {
                    "role": "user",
                    "parts": [{"kind": "text", "text": "pay bob"}]
                },
                "metadata": metadata,
            })),
            id: Id::Number(7),
        }
    }

    fn store_and_minter() -> (
        crate::a2a::jsonrpc_dispatch::InMemoryTaskStore,
        CounterMinter,
    ) {
        (
            crate::a2a::jsonrpc_dispatch::InMemoryTaskStore::new(),
            CounterMinter(AtomicU64::new(0)),
        )
    }

    // ---- positive paths ---------------------------------------------------

    #[test]
    fn no_metadata_field_passes_through_to_underlying_handler() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let trust = make_trust_store("kid-a", &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let r = req_with_metadata(None);
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        assert!(resp.error.is_none(), "expected ok, got {:?}", resp.error);
        let result = resp.result.expect("result");
        assert_eq!(result["state"], "submitted");
        assert!(ledger.snapshot().is_empty(), "ledger untouched");
    }

    #[test]
    fn metadata_without_ap2_key_passes_through() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let trust = make_trust_store("kid-a", &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let r = req_with_metadata(Some(json!({"unrelated": "value"})));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        assert!(resp.error.is_none());
        assert!(ledger.snapshot().is_empty());
    }

    #[test]
    fn signed_valid_ap2_extension_records_to_ledger_and_creates_task() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let kid = "kid-a";
        let trust = make_trust_store(kid, &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let mandate = sign(baseline_mandate("m1", kid), &sk, kid);
        let attempt = baseline_attempt("m1", "n1");

        let r = req_with_metadata(Some(json!({
            "ap2": {"mandate": mandate, "attempt": attempt}
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        assert!(resp.error.is_none(), "denial: {:?}", resp.error);
        assert_eq!(ledger.snapshot().len(), 1, "ledger appended");
        assert_eq!(ledger.snapshot()[0].transfer_nonce, "n1");
    }

    // ---- signature negative paths ----------------------------------------

    #[test]
    fn unsigned_mandate_rejected_with_ap2_denied() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let trust = make_trust_store("kid-a", &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let mandate = baseline_mandate("m1", "kid-a"); // unsigned
        let attempt = baseline_attempt("m1", "n1");
        let r = req_with_metadata(Some(json!({
            "ap2": {"mandate": mandate, "attempt": attempt}
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("denied");
        assert_eq!(err.code, i32::from(A2aErrorCode::Ap2Denied));
        assert_eq!(err.data.unwrap()["kind"], "mandateUnauthentic");
        assert!(ledger.snapshot().is_empty(), "ledger untouched on denial");
    }

    #[test]
    fn signed_by_unknown_kid_rejected() {
        let issuer_sk = SigningKey::from_bytes(&[7; 32]);
        let trust = make_trust_store("kid-known", &issuer_sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        // Sign with same key but different kid — kid not in store.
        let mandate = sign(baseline_mandate("m1", "kid-other"), &issuer_sk, "kid-other");
        let attempt = baseline_attempt("m1", "n1");
        let r = req_with_metadata(Some(json!({
            "ap2": {"mandate": mandate, "attempt": attempt}
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("denied");
        assert_eq!(err.data.unwrap()["kind"], "mandateUnauthentic");
        assert!(ledger.snapshot().is_empty());
    }

    #[test]
    fn tampered_payload_after_signing_rejected() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let kid = "kid-a";
        let trust = make_trust_store(kid, &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let mut mandate = sign(baseline_mandate("m1", kid), &sk, kid);
        // Tamper with mandate body after signing.
        mandate.daily_cap = mandate.daily_cap.saturating_mul(2);
        let attempt = baseline_attempt("m1", "n1");
        let r = req_with_metadata(Some(json!({
            "ap2": {"mandate": mandate, "attempt": attempt}
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("denied");
        assert_eq!(err.data.unwrap()["kind"], "mandateUnauthentic");
    }

    #[test]
    fn empty_trust_store_fails_closed_for_signed_mandate() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let kid = "kid-a";
        let trust = MandateTrustStore::new(); // empty
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let mandate = sign(baseline_mandate("m1", kid), &sk, kid);
        let attempt = baseline_attempt("m1", "n1");
        let r = req_with_metadata(Some(json!({
            "ap2": {"mandate": mandate, "attempt": attempt}
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("denied");
        assert_eq!(err.data.unwrap()["kind"], "mandateUnauthentic");
    }

    #[test]
    fn expired_anchor_rejects_signed_mandate() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let kid = "kid-a";
        // Anchor expires at 1_700_000_000 (== now → strict expiry)
        let mut b = TrustStoreBuilder::new().generation(1);
        b.add(TrustAnchor {
            kid: kid.into(),
            key: sk.verifying_key(),
            not_after: Some(1_700_000_000),
            source: AnchorSource::from("mandate-issuer-cr/test"),
        })
        .unwrap();
        let trust = MandateTrustStore::new();
        trust.replace_snapshot(MandateTrustStoreSnapshot::from_inner(b.build()));

        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let mandate = sign(baseline_mandate("m1", kid), &sk, kid);
        let attempt = baseline_attempt("m1", "n1");
        let r = req_with_metadata(Some(json!({
            "ap2": {"mandate": mandate, "attempt": attempt}
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("denied");
        assert_eq!(err.data.unwrap()["kind"], "mandateUnauthentic");
    }

    // ---- policy negative paths -------------------------------------------

    #[test]
    fn cap_exceeded_rejected_after_signature_passes() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let kid = "kid-a";
        let trust = make_trust_store(kid, &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let mut mandate_unsigned = baseline_mandate("m1", kid);
        mandate_unsigned.per_transfer_cap = 50; // attempt is 100 — over cap
        let mandate = sign(mandate_unsigned, &sk, kid);
        let attempt = baseline_attempt("m1", "n1");
        let r = req_with_metadata(Some(json!({
            "ap2": {"mandate": mandate, "attempt": attempt}
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("denied");
        assert_eq!(err.code, i32::from(A2aErrorCode::Ap2Denied));
        assert_eq!(err.data.unwrap()["kind"], "perTransferCapExceeded");
        assert!(
            ledger.snapshot().is_empty(),
            "ledger MUST NOT be appended on denial"
        );
    }

    #[test]
    fn replayed_nonce_rejected() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let kid = "kid-a";
        let trust = make_trust_store(kid, &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        // Pre-seed ledger with the nonce.
        ledger.record(PaymentRecord {
            mandate_id: "m1".into(),
            counterparty: "did:example:bob".into(),
            amount: 50,
            currency: "USD".into(),
            transfer_nonce: "n1".into(),
            timestamp: 1_700_000_000,
        });

        let mandate = sign(baseline_mandate("m1", kid), &sk, kid);
        let attempt = baseline_attempt("m1", "n1");
        let r = req_with_metadata(Some(json!({
            "ap2": {"mandate": mandate, "attempt": attempt}
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("denied");
        assert_eq!(err.data.unwrap()["kind"], "replayDetected");
    }

    // ---- malformed extension --------------------------------------------

    #[test]
    fn malformed_ap2_extension_rejected_as_invalid_params() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let trust = make_trust_store("kid-a", &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        // ap2 present but wrong shape
        let r = req_with_metadata(Some(json!({"ap2": {"mandate": "not-an-object"}})));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("invalid");
        assert_eq!(err.code, crate::mcp::error::ErrorCode::InvalidParams.code());
        assert!(err.data.unwrap()["reason"]
            .as_str()
            .unwrap()
            .starts_with("metadata.ap2:"));
    }

    #[test]
    fn ap2_with_extra_fields_rejected_due_to_deny_unknown_fields() {
        let sk = SigningKey::from_bytes(&[7; 32]);
        let trust = make_trust_store("kid-a", &sk);
        let mut ledger = InMemoryMandateLedger::new();
        let (store, minter) = store_and_minter();

        let mandate = sign(baseline_mandate("m1", "kid-a"), &sk, "kid-a");
        let attempt = baseline_attempt("m1", "n1");
        let r = req_with_metadata(Some(json!({
            "ap2": {
                "mandate": mandate,
                "attempt": attempt,
                "rogue": "field",
            }
        })));
        let resp = handle_message_send_with_ap2(
            &r,
            &store,
            &minter,
            &trust,
            &mut ledger,
            1_700_000_000,
        );
        let err = resp.error.expect("invalid");
        assert_eq!(err.code, crate::mcp::error::ErrorCode::InvalidParams.code());
    }

    // ---- denial-kind exhaustiveness -------------------------------------

    #[test]
    fn denial_kind_string_is_not_default_for_known_variants() {
        // Spot-check a few — exhaustive coverage achieved via the
        // match statement which fails to compile if a variant is
        // missed. (Just verifies the strings are non-empty so a
        // future drop to "" by accident is caught.)
        let cases = [
            Ap2Denial::MandateUnauthentic("x".into()),
            Ap2Denial::AmountZero,
            Ap2Denial::CounterpartyNotAllowed { counterparty: "c".into() },
        ];
        for d in &cases {
            assert!(!denial_kind_for_test(d).is_empty());
        }
    }

    // ---- silence unused-imports for engine alias --------------------------

    #[test]
    fn base64_engine_alias_used_via_extraction() {
        // Belt-and-braces: confirms our base64 engine import is
        // retained for any future use even if rustc's dead-code
        // analysis briefly flags it.
        let _ = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([0u8; 4]);
    }
}
