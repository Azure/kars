// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! AP2 conformance corpus — fixture-driven integration tests.
//!
//! Each `tests/fixtures/ap2_conformance/NNN-*.json` file describes a
//! self-contained scenario (mandate + ledger pre-state + attempt + clock
//! + expected verdict). This test loads every fixture, drives
//!   [`azureclaw_inference_router::a2a::validate_payment_attempt`], and
//!   asserts the verdict matches.
//!
//! Why a separate corpus file rather than inline `#[test]`s?
//!
//! - Fixtures are wire-format JSON (camelCase per A2A 1.0.0); when AGT
//!   ships its own AP2 evaluator we can drive *both* implementations
//!   from the same corpus and compare.
//! - New denial scenarios can be added by dropping a JSON file and
//!   re-running — no Rust recompile of the test harness.
//! - The corpus is the conformance-corpus contribution called out in
//!   internal Phase 1 plan §5.4 row "AP2 commerce" (positive +
//!   negative cases).
//!
//! Add a fixture by:
//!
//! 1. Drop `NNN-name.json` into `tests/fixtures/ap2_conformance/`.
//! 2. `expected.verdict` must be `"allow"` or `"deny"`; for `"deny"`
//!    add `expected.kind` matching one of the [`Ap2Denial`] variants.
//! 3. `cargo test -p azureclaw-inference-router --test ap2_conformance`.

use std::fs;
use std::path::{Path, PathBuf};

use azureclaw_inference_router::a2a::{
    Ap2Denial, InMemoryMandateLedger, IntentMandate, MandateLedgerMut, PaymentAttempt,
    PaymentRecord, validate_payment_attempt,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    name: String,
    now: i64,
    mandate: IntentMandate,
    ledger: Vec<PaymentRecord>,
    attempt: PaymentAttempt,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, tag = "verdict", rename_all = "lowercase")]
enum Expected {
    Allow,
    Deny { kind: String },
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("ap2_conformance")
}

fn load_fixtures() -> Vec<(String, Fixture)> {
    let dir = fixtures_dir();
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read fixtures dir {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.ends_with(".json") && !s.starts_with('_')
        })
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
        let fx: Fixture = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()));
        out.push((path.display().to_string(), fx));
    }
    assert!(!out.is_empty(), "no fixtures found in {}", dir.display());
    out
}

fn ledger_from_records(records: &[PaymentRecord]) -> InMemoryMandateLedger {
    let mut ledger = InMemoryMandateLedger::new();
    for r in records {
        ledger.record(r.clone());
    }
    ledger
}

fn denial_kind(d: &Ap2Denial) -> &'static str {
    match d {
        Ap2Denial::MandateIdMismatch => "MandateIdMismatch",
        Ap2Denial::MandateExpired { .. } => "MandateExpired",
        Ap2Denial::CurrencyMismatch { .. } => "CurrencyMismatch",
        Ap2Denial::AmountZero => "AmountZero",
        Ap2Denial::PerTransferCapExceeded { .. } => "PerTransferCapExceeded",
        Ap2Denial::CounterpartyNotAllowed { .. } => "CounterpartyNotAllowed",
        Ap2Denial::DailyCapExceeded { .. } => "DailyCapExceeded",
        Ap2Denial::MonthlyCapExceeded { .. } => "MonthlyCapExceeded",
        Ap2Denial::ReplayDetected { .. } => "ReplayDetected",
        Ap2Denial::AttemptInFuture { .. } => "AttemptInFuture",
        Ap2Denial::AttemptTooOld { .. } => "AttemptTooOld",
        Ap2Denial::ArithmeticOverflow => "ArithmeticOverflow",
        Ap2Denial::MandateUnauthentic(_) => "MandateUnauthentic",
    }
}

#[test]
fn ap2_conformance_corpus() {
    let fixtures = load_fixtures();
    for (path, fx) in &fixtures {
        let ledger = ledger_from_records(&fx.ledger);
        let result = validate_payment_attempt(&fx.mandate, &fx.attempt, &ledger, fx.now);
        match (&fx.expected, &result) {
            (Expected::Allow, Ok(_)) => {}
            (Expected::Allow, Err(e)) => {
                panic!(
                    "fixture {} ({}): expected Allow but got Deny({:?})",
                    fx.name, path, e
                );
            }
            (Expected::Deny { kind }, Err(d)) => {
                let actual_kind = denial_kind(d);
                assert_eq!(
                    kind, actual_kind,
                    "fixture {} ({}): expected Deny.{} but got Deny.{} ({:?})",
                    fx.name, path, kind, actual_kind, d
                );
            }
            (Expected::Deny { kind }, Ok(_)) => {
                panic!(
                    "fixture {} ({}): expected Deny.{} but got Allow",
                    fx.name, path, kind
                );
            }
        }
    }
}

#[test]
fn ap2_conformance_corpus_has_minimum_coverage() {
    // Guard rail: corpus must cover all 11 reachable denial kinds plus
    // at least one positive case. ArithmeticOverflow is unreachable
    // without u64 arithmetic exceeding 2^64-1 in the ledger sum, which
    // requires synthetic input we can add later.
    let fixtures = load_fixtures();
    let mut seen_allow = false;
    let mut seen_deny: std::collections::BTreeSet<String> = Default::default();
    for (_, fx) in &fixtures {
        match &fx.expected {
            Expected::Allow => seen_allow = true,
            Expected::Deny { kind } => {
                seen_deny.insert(kind.clone());
            }
        }
    }
    assert!(seen_allow, "corpus missing at least one Allow fixture");
    let required: &[&str] = &[
        "MandateIdMismatch",
        "MandateExpired",
        "CurrencyMismatch",
        "AmountZero",
        "PerTransferCapExceeded",
        "CounterpartyNotAllowed",
        "DailyCapExceeded",
        "MonthlyCapExceeded",
        "ReplayDetected",
        "AttemptInFuture",
        "AttemptTooOld",
    ];
    for kind in required {
        assert!(
            seen_deny.contains(*kind),
            "corpus missing Deny.{kind} fixture; current: {:?}",
            seen_deny
        );
    }
}
