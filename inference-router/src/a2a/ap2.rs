//! A2A 1.0.0 — AP2 (Agent Payments) commerce-mandate types and validation.
//!
//! This module implements the *evaluation* half of AP2: given an
//! already-cryptographically-verified [`IntentMandate`] (the signed
//! authorisation that a principal has issued to an agent allowing it to
//! transact within a documented envelope) and a proposed [`PaymentAttempt`]
//! (the candidate transfer the agent wants to perform), determine whether
//! the attempt is permitted.
//!
//! It is a **pure function** — no I/O, no clock except an explicit
//! `now: i64` parameter, no signature verification (that is
//! [`crate::a2a::card_verifier`]'s job). Callers provide:
//!
//! 1. The verified [`IntentMandate`] (signature-checked upstream).
//! 2. The current [`PaymentAttempt`].
//! 3. A [`MandateLedger`] view over already-recorded
//!    [`PaymentRecord`]s for the same mandate.
//! 4. The current Unix timestamp (seconds since epoch).
//!
//! Validation enforces, in order:
//!
//! - mandate not expired (`exp` ≥ `now`),
//! - currency matches,
//! - per-attempt cap not exceeded,
//! - counterparty in allowlist (`*` wildcard supported as a single entry
//!   meaning "any counterparty"),
//! - daily cap not exceeded (rolling 24 h sum from records ≤ `daily_cap`),
//! - monthly cap not exceeded (rolling 30 d sum from records ≤
//!   `monthly_cap`),
//! - transfer nonce not previously seen (replay protection).
//!
//! Failure variants are [`Ap2Denial`]; the caller maps them onto
//! application-level JSON-RPC errors and emits an audit event.
//!
//! ## Authority and route binding
//!
//! This module does **not** itself sign mandates, sign payment receipts,
//! call AGT, or talk to a real payment rail. It is the policy-evaluation
//! kernel that future router routes will call after verifying signatures
//! via [`crate::a2a::card_verifier`] and consulting the
//! [`crate::governance::PolicyDecisionProvider`]. Cluster-validated route
//! binding lands in a separate PR.
//!
//! ## Spec
//!
//! Field shapes derive from the A2A 1.0.0 AP2 extension. Mandate amounts
//! are expressed as **minor units** (cents, fils, etc.), never as
//! floating-point — the type is `u64` to make negative or fractional
//! amounts unrepresentable.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Wildcard counterparty identifier permitting any merchant.
///
/// When [`IntentMandate::counterparty_allowlist`] contains exactly the
/// single entry `"*"`, every counterparty matches. Any non-wildcard
/// entries alongside `"*"` are still required to be exact-match strings.
pub const COUNTERPARTY_WILDCARD: &str = "*";

/// 24 hours expressed in seconds — the daily-cap window.
pub const DAILY_WINDOW_SECS: i64 = 24 * 60 * 60;

/// 30 days expressed in seconds — the monthly-cap window.
pub const MONTHLY_WINDOW_SECS: i64 = 30 * 24 * 60 * 60;

/// Signed authorisation a principal has issued to an agent permitting it
/// to transact within a documented envelope.
///
/// Field naming mirrors the AP2 extension wire format (camelCase). The
/// `signature` field is **opaque** to this module — it is verified
/// upstream and propagates through unchanged so audit consumers can see
/// the mandate identity that authorised a given record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IntentMandate {
    /// Stable mandate identifier (issuer-scoped). Used as a foreign key
    /// in [`PaymentRecord::mandate_id`].
    pub mandate_id: String,
    /// Subject (the agent or principal the mandate is issued *to*) as an
    /// opaque string identifier.
    pub principal: String,
    /// ISO 4217 currency code (e.g. `"USD"`). Must equal
    /// [`PaymentAttempt::currency`].
    pub currency: String,
    /// Maximum cumulative amount (minor units) permitted across any
    /// rolling 24-hour window. Set to `u64::MAX` to disable.
    pub daily_cap: u64,
    /// Maximum cumulative amount (minor units) permitted across any
    /// rolling 30-day window. Set to `u64::MAX` to disable.
    pub monthly_cap: u64,
    /// Maximum amount (minor units) for any single transfer. Set to
    /// `u64::MAX` to disable.
    pub per_transfer_cap: u64,
    /// Allowlist of counterparty identifiers. The single-element list
    /// `["*"]` matches any counterparty (see [`COUNTERPARTY_WILDCARD`]).
    pub counterparty_allowlist: BTreeSet<String>,
    /// Mandate expiry as Unix epoch seconds. The mandate is **invalid
    /// at and beyond** this timestamp (the comparison is `now < exp`).
    pub exp: i64,
    /// Opaque signature blob, base64-encoded. This module never inspects
    /// the value; it is included so audit records carry the issuer's
    /// signature alongside the resulting [`PaymentRecord`].
    pub signature: String,
}

/// A candidate transfer the agent wishes to perform under [`IntentMandate`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PaymentAttempt {
    /// Foreign key into the mandate that authorises this attempt.
    pub mandate_id: String,
    /// Counterparty identifier (must be in
    /// [`IntentMandate::counterparty_allowlist`] or that allowlist must
    /// be `["*"]`).
    pub counterparty: String,
    /// Transfer amount in minor units of [`PaymentAttempt::currency`].
    /// Must be non-zero and ≤ [`IntentMandate::per_transfer_cap`].
    pub amount: u64,
    /// ISO 4217 currency code (must equal [`IntentMandate::currency`]).
    pub currency: String,
    /// Caller-generated transfer nonce. Unique per attempt; the validator
    /// rejects if any prior [`PaymentRecord`] in the same mandate already
    /// has this nonce (replay protection).
    pub transfer_nonce: String,
    /// Caller-provided proposed timestamp (Unix epoch seconds). Must be
    /// `<= now` and not absurdly in the past (see
    /// [`Ap2Denial::AttemptInFuture`] / [`Ap2Denial::AttemptTooOld`]).
    pub timestamp: i64,
}

/// Permanent record of a successfully validated [`PaymentAttempt`].
///
/// Persisted in the mandate's [`MandateLedger`] so subsequent attempts
/// can be checked against running daily/monthly totals and replay
/// nonces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PaymentRecord {
    pub mandate_id: String,
    pub counterparty: String,
    pub amount: u64,
    pub currency: String,
    pub transfer_nonce: String,
    pub timestamp: i64,
}

impl PaymentRecord {
    /// Project a successfully validated [`PaymentAttempt`] onto its
    /// permanent ledger record. Caller is expected to persist the
    /// returned value via [`MandateLedger::record`] (the validator does
    /// not mutate state).
    #[must_use]
    pub fn from_attempt(attempt: &PaymentAttempt) -> Self {
        Self {
            mandate_id: attempt.mandate_id.clone(),
            counterparty: attempt.counterparty.clone(),
            amount: attempt.amount,
            currency: attempt.currency.clone(),
            transfer_nonce: attempt.transfer_nonce.clone(),
            timestamp: attempt.timestamp,
        }
    }
}

/// Read-only view of all [`PaymentRecord`]s already on file for one
/// mandate. The validator queries this trait for replay/window checks.
///
/// Implementations must be cheap to call repeatedly (the validator may
/// invoke each method once per evaluation). [`InMemoryMandateLedger`]
/// is the in-tree reference implementation; production deployments are
/// expected to back this with an AGT-side store keyed by `mandate_id`.
pub trait MandateLedger {
    /// Sum of `amount` over records strictly newer than
    /// `now - DAILY_WINDOW_SECS` for the given mandate.
    fn sum_in_window(&self, mandate_id: &str, since_inclusive: i64) -> u64;
    /// Whether a record with this `(mandate_id, transfer_nonce)` already
    /// exists. Used for replay protection.
    fn nonce_seen(&self, mandate_id: &str, nonce: &str) -> bool;
}

/// Mutable extension of [`MandateLedger`]; the validator never calls
/// this — only the wrapping route handler, *after* validation succeeds.
pub trait MandateLedgerMut: MandateLedger {
    /// Persist a successful payment. Must be idempotent on
    /// `(mandate_id, transfer_nonce)`: a second insert with an existing
    /// pair is a no-op.
    fn record(&mut self, record: PaymentRecord);
}

/// Reference [`MandateLedger`] for tests and the in-tree reference path.
///
/// Stores records in a flat `Vec` keyed off `mandate_id`. Window queries
/// are `O(n)` per call which is fine for the in-process ledger; AGT-side
/// implementations should index appropriately.
#[derive(Default, Debug, Clone)]
pub struct InMemoryMandateLedger {
    records: Vec<PaymentRecord>,
}

impl InMemoryMandateLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot accessor (clones). Useful for tests + audit projection.
    #[must_use]
    pub fn snapshot(&self) -> Vec<PaymentRecord> {
        self.records.clone()
    }
}

impl MandateLedger for InMemoryMandateLedger {
    fn sum_in_window(&self, mandate_id: &str, since_inclusive: i64) -> u64 {
        self.records
            .iter()
            .filter(|r| r.mandate_id == mandate_id && r.timestamp >= since_inclusive)
            .fold(0u64, |acc, r| acc.saturating_add(r.amount))
    }

    fn nonce_seen(&self, mandate_id: &str, nonce: &str) -> bool {
        self.records
            .iter()
            .any(|r| r.mandate_id == mandate_id && r.transfer_nonce == nonce)
    }
}

impl MandateLedgerMut for InMemoryMandateLedger {
    fn record(&mut self, record: PaymentRecord) {
        if self.nonce_seen(&record.mandate_id, &record.transfer_nonce) {
            return;
        }
        self.records.push(record);
    }
}

/// All ways a [`PaymentAttempt`] can be rejected. Ordered roughly from
/// "obvious malformedness" to "policy violation"; tests exercise each
/// variant explicitly.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum Ap2Denial {
    #[error("payment attempt mandate_id does not match the supplied mandate")]
    MandateIdMismatch,
    #[error("mandate has expired (exp={exp}, now={now})")]
    MandateExpired { exp: i64, now: i64 },
    #[error("mandate currency '{mandate}' does not match attempt currency '{attempt}'")]
    CurrencyMismatch { mandate: String, attempt: String },
    #[error("payment attempt amount must be non-zero")]
    AmountZero,
    #[error("attempt amount {amount} exceeds per-transfer cap {cap}")]
    PerTransferCapExceeded { amount: u64, cap: u64 },
    #[error("counterparty '{counterparty}' is not in the mandate allowlist")]
    CounterpartyNotAllowed { counterparty: String },
    #[error("daily cap {cap} would be exceeded (existing {existing} + attempt {attempt})")]
    DailyCapExceeded {
        cap: u64,
        existing: u64,
        attempt: u64,
    },
    #[error("monthly cap {cap} would be exceeded (existing {existing} + attempt {attempt})")]
    MonthlyCapExceeded {
        cap: u64,
        existing: u64,
        attempt: u64,
    },
    #[error("transfer nonce '{nonce}' has already been used for this mandate")]
    ReplayDetected { nonce: String },
    #[error("attempt timestamp {ts} is in the future (now={now})")]
    AttemptInFuture { ts: i64, now: i64 },
    #[error("attempt timestamp {ts} is older than the monthly window (now={now})")]
    AttemptTooOld { ts: i64, now: i64 },
    #[error("attempt total would overflow u64 sum")]
    ArithmeticOverflow,
}

/// Validate a [`PaymentAttempt`] against an [`IntentMandate`] and the
/// recorded ledger.
///
/// `now` is the current Unix epoch seconds; callers are expected to
/// source it from a single, monotonic clock so retries on the same
/// attempt give deterministic answers.
///
/// On success, returns the [`PaymentRecord`] the caller should
/// subsequently persist via [`MandateLedgerMut::record`]. On failure,
/// returns the structured [`Ap2Denial`] and the ledger is left
/// untouched.
///
/// # Errors
///
/// Returns [`Ap2Denial`] when any of the documented validation checks
/// fail. The function performs all checks in a documented order; the
/// first-failing check determines the returned variant.
pub fn validate_payment_attempt(
    mandate: &IntentMandate,
    attempt: &PaymentAttempt,
    ledger: &dyn MandateLedger,
    now: i64,
) -> Result<PaymentRecord, Ap2Denial> {
    if mandate.mandate_id != attempt.mandate_id {
        return Err(Ap2Denial::MandateIdMismatch);
    }
    if now >= mandate.exp {
        return Err(Ap2Denial::MandateExpired {
            exp: mandate.exp,
            now,
        });
    }
    if mandate.currency != attempt.currency {
        return Err(Ap2Denial::CurrencyMismatch {
            mandate: mandate.currency.clone(),
            attempt: attempt.currency.clone(),
        });
    }
    if attempt.amount == 0 {
        return Err(Ap2Denial::AmountZero);
    }
    if attempt.amount > mandate.per_transfer_cap {
        return Err(Ap2Denial::PerTransferCapExceeded {
            amount: attempt.amount,
            cap: mandate.per_transfer_cap,
        });
    }

    if attempt.timestamp > now {
        return Err(Ap2Denial::AttemptInFuture {
            ts: attempt.timestamp,
            now,
        });
    }
    if attempt.timestamp < now.saturating_sub(MONTHLY_WINDOW_SECS) {
        return Err(Ap2Denial::AttemptTooOld {
            ts: attempt.timestamp,
            now,
        });
    }

    if !counterparty_allowed(mandate, &attempt.counterparty) {
        return Err(Ap2Denial::CounterpartyNotAllowed {
            counterparty: attempt.counterparty.clone(),
        });
    }

    if ledger.nonce_seen(&mandate.mandate_id, &attempt.transfer_nonce) {
        return Err(Ap2Denial::ReplayDetected {
            nonce: attempt.transfer_nonce.clone(),
        });
    }

    let daily_existing = ledger.sum_in_window(
        &mandate.mandate_id,
        now.saturating_sub(DAILY_WINDOW_SECS),
    );
    let daily_total = daily_existing
        .checked_add(attempt.amount)
        .ok_or(Ap2Denial::ArithmeticOverflow)?;
    if daily_total > mandate.daily_cap {
        return Err(Ap2Denial::DailyCapExceeded {
            cap: mandate.daily_cap,
            existing: daily_existing,
            attempt: attempt.amount,
        });
    }

    let monthly_existing = ledger.sum_in_window(
        &mandate.mandate_id,
        now.saturating_sub(MONTHLY_WINDOW_SECS),
    );
    let monthly_total = monthly_existing
        .checked_add(attempt.amount)
        .ok_or(Ap2Denial::ArithmeticOverflow)?;
    if monthly_total > mandate.monthly_cap {
        return Err(Ap2Denial::MonthlyCapExceeded {
            cap: mandate.monthly_cap,
            existing: monthly_existing,
            attempt: attempt.amount,
        });
    }

    Ok(PaymentRecord::from_attempt(attempt))
}

fn counterparty_allowed(mandate: &IntentMandate, counterparty: &str) -> bool {
    if mandate.counterparty_allowlist.len() == 1
        && mandate
            .counterparty_allowlist
            .contains(COUNTERPARTY_WILDCARD)
    {
        return true;
    }
    mandate.counterparty_allowlist.contains(counterparty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    fn baseline_mandate() -> IntentMandate {
        IntentMandate {
            mandate_id: "mandate-1".into(),
            principal: "agent-7".into(),
            currency: "USD".into(),
            daily_cap: 10_000,
            monthly_cap: 100_000,
            per_transfer_cap: 5_000,
            counterparty_allowlist: allowlist(&["acme-corp", "globex"]),
            exp: 2_000_000_000,
            signature: "sig-blob".into(),
        }
    }

    fn baseline_attempt() -> PaymentAttempt {
        PaymentAttempt {
            mandate_id: "mandate-1".into(),
            counterparty: "acme-corp".into(),
            amount: 500,
            currency: "USD".into(),
            transfer_nonce: "nonce-1".into(),
            timestamp: 1_700_000_000,
        }
    }

    #[test]
    fn happy_path_returns_record_for_persistence() {
        let mandate = baseline_mandate();
        let attempt = baseline_attempt();
        let ledger = InMemoryMandateLedger::new();
        let rec = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap();
        assert_eq!(rec.amount, 500);
        assert_eq!(rec.transfer_nonce, "nonce-1");
        assert_eq!(rec.mandate_id, "mandate-1");
    }

    #[test]
    fn mandate_id_mismatch_is_first_check() {
        let mandate = baseline_mandate();
        let mut attempt = baseline_attempt();
        attempt.mandate_id = "mandate-2".into();
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert_eq!(err, Ap2Denial::MandateIdMismatch);
    }

    #[test]
    fn expired_mandate_is_rejected() {
        let mut mandate = baseline_mandate();
        mandate.exp = 1_600_000_000;
        let attempt = baseline_attempt();
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::MandateExpired { .. }));
    }

    #[test]
    fn currency_mismatch_is_rejected() {
        let mandate = baseline_mandate();
        let mut attempt = baseline_attempt();
        attempt.currency = "EUR".into();
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::CurrencyMismatch { .. }));
    }

    #[test]
    fn zero_amount_rejected() {
        let mandate = baseline_mandate();
        let mut attempt = baseline_attempt();
        attempt.amount = 0;
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert_eq!(err, Ap2Denial::AmountZero);
    }

    #[test]
    fn per_transfer_cap_exceeded_rejected() {
        let mandate = baseline_mandate();
        let mut attempt = baseline_attempt();
        attempt.amount = 9_000;
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::PerTransferCapExceeded { .. }));
    }

    #[test]
    fn counterparty_not_in_allowlist_rejected() {
        let mandate = baseline_mandate();
        let mut attempt = baseline_attempt();
        attempt.counterparty = "shady-shop".into();
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::CounterpartyNotAllowed { .. }));
    }

    #[test]
    fn wildcard_allowlist_permits_any_counterparty() {
        let mut mandate = baseline_mandate();
        mandate.counterparty_allowlist = allowlist(&["*"]);
        let mut attempt = baseline_attempt();
        attempt.counterparty = "literally-anyone".into();
        let ledger = InMemoryMandateLedger::new();
        validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap();
    }

    #[test]
    fn wildcard_must_be_sole_entry_to_match_arbitrary() {
        let mut mandate = baseline_mandate();
        mandate.counterparty_allowlist = allowlist(&["*", "acme-corp"]);
        let mut attempt = baseline_attempt();
        attempt.counterparty = "literally-anyone".into();
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::CounterpartyNotAllowed { .. }));
    }

    #[test]
    fn daily_cap_exceeded_rejects() {
        let mandate = baseline_mandate();
        let mut ledger = InMemoryMandateLedger::new();
        ledger.record(PaymentRecord {
            mandate_id: "mandate-1".into(),
            counterparty: "acme-corp".into(),
            amount: 9_800,
            currency: "USD".into(),
            transfer_nonce: "earlier".into(),
            timestamp: 1_699_990_000,
        });
        let attempt = baseline_attempt();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::DailyCapExceeded { .. }));
    }

    #[test]
    fn daily_cap_resets_after_window() {
        let mandate = baseline_mandate();
        let mut ledger = InMemoryMandateLedger::new();
        ledger.record(PaymentRecord {
            mandate_id: "mandate-1".into(),
            counterparty: "acme-corp".into(),
            amount: 9_800,
            currency: "USD".into(),
            transfer_nonce: "earlier".into(),
            timestamp: 1_700_000_000 - DAILY_WINDOW_SECS - 1,
        });
        let attempt = baseline_attempt();
        validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap();
    }

    #[test]
    fn monthly_cap_exceeded_rejects() {
        let mandate = baseline_mandate();
        let mut ledger = InMemoryMandateLedger::new();
        for i in 0..20 {
            ledger.record(PaymentRecord {
                mandate_id: "mandate-1".into(),
                counterparty: "acme-corp".into(),
                amount: 4_999,
                currency: "USD".into(),
                transfer_nonce: format!("n-{i}"),
                timestamp: 1_700_000_000 - DAILY_WINDOW_SECS - 100 - (i as i64),
            });
        }
        let attempt = baseline_attempt();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::MonthlyCapExceeded { .. }));
    }

    #[test]
    fn replay_nonce_rejected() {
        let mandate = baseline_mandate();
        let mut ledger = InMemoryMandateLedger::new();
        ledger.record(PaymentRecord {
            mandate_id: "mandate-1".into(),
            counterparty: "acme-corp".into(),
            amount: 1,
            currency: "USD".into(),
            transfer_nonce: "nonce-1".into(),
            timestamp: 1_699_999_000,
        });
        let attempt = baseline_attempt();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::ReplayDetected { .. }));
    }

    #[test]
    fn attempt_in_future_rejected() {
        let mandate = baseline_mandate();
        let mut attempt = baseline_attempt();
        attempt.timestamp = 1_700_000_001;
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::AttemptInFuture { .. }));
    }

    #[test]
    fn attempt_too_old_rejected() {
        let mandate = baseline_mandate();
        let mut attempt = baseline_attempt();
        attempt.timestamp = 1_700_000_000 - MONTHLY_WINDOW_SECS - 1;
        let ledger = InMemoryMandateLedger::new();
        let err = validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap_err();
        assert!(matches!(err, Ap2Denial::AttemptTooOld { .. }));
    }

    #[test]
    fn ledger_record_idempotent_on_nonce() {
        let mut ledger = InMemoryMandateLedger::new();
        let rec = PaymentRecord {
            mandate_id: "m".into(),
            counterparty: "c".into(),
            amount: 10,
            currency: "USD".into(),
            transfer_nonce: "x".into(),
            timestamp: 100,
        };
        ledger.record(rec.clone());
        ledger.record(rec);
        assert_eq!(ledger.snapshot().len(), 1);
    }

    #[test]
    fn cap_disabled_with_u64_max() {
        let mut mandate = baseline_mandate();
        mandate.daily_cap = u64::MAX;
        mandate.monthly_cap = u64::MAX;
        mandate.per_transfer_cap = u64::MAX;
        let mut attempt = baseline_attempt();
        attempt.amount = 1_000_000_000_000;
        let ledger = InMemoryMandateLedger::new();
        validate_payment_attempt(&mandate, &attempt, &ledger, 1_700_000_000).unwrap();
    }

    #[test]
    fn json_round_trip_camel_case() {
        let m = baseline_mandate();
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"mandateId\""));
        assert!(s.contains("\"counterpartyAllowlist\""));
        assert!(s.contains("\"perTransferCap\""));
        let m2: IntentMandate = serde_json::from_str(&s).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn unknown_field_rejected() {
        let json = r#"{
            "mandateId": "m",
            "principal": "p",
            "currency": "USD",
            "dailyCap": 1,
            "monthlyCap": 1,
            "perTransferCap": 1,
            "counterpartyAllowlist": [],
            "exp": 1,
            "signature": "s",
            "extra": "x"
        }"#;
        assert!(serde_json::from_str::<IntentMandate>(json).is_err());
    }
}
