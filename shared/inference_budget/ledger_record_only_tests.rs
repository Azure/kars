// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn fixture() -> (Ledger, ReserveRequest) {
    let mut ledger = ledger(100);
    ledger.limits = Limits::default();
    ledger.nodes.get_mut("root").unwrap().authority.limits = Limits::default();
    let mut child = task("child", Some("root"), 0);
    child.limits = Limits::default();
    let ledger = ledger.register_task(child).unwrap().next;
    let mut request = request("child", "record-pod", 1);
    request.quote = AttemptQuote::record_only(
        "provider",
        "https://provider.example/v1",
        "uncontracted-model",
        Operation::Responses,
    );
    let ledger = ledger
        .register_session(request.identity.clone())
        .unwrap()
        .next;
    (ledger, request)
}

fn inflight(ledger: Ledger, request: &ReserveRequest) -> Ledger {
    ledger
        .reserve(request, 100)
        .unwrap()
        .next
        .begin_dispatch(&command(request), 101)
        .unwrap()
        .next
}

fn restart(ledger: &Ledger) -> Ledger {
    let decoded: Ledger = serde_json::from_slice(&serde_json::to_vec(ledger).unwrap()).unwrap();
    decoded.validate().unwrap();
    assert_eq!(&decoded, ledger);
    decoded
}

fn large_usage() -> Usage {
    Usage {
        input_tokens: 1_000_000,
        output_tokens: 2_000_000,
        cached_input_tokens: 100_000,
        cache_creation_input_tokens: 200_000,
        reasoning_output_tokens: 1_000_000,
    }
}

#[test]
fn record_only_old_bounded_wire_is_unchanged_and_new_wire_is_strict() {
    let request = request("root", "a", 1);
    let bounded = request.quote.bounded().unwrap();
    assert_eq!(
        serde_json::to_value(&request.quote).unwrap(),
        serde_json::to_value(bounded).unwrap()
    );
    let expiry = chrono::DateTime::parse_from_rfc3339(&bounded.contract.valid_until)
        .unwrap()
        .timestamp();
    assert_eq!(request.quote.expires_at(expiry - 1).unwrap(), expiry);
    let old = serde_json::to_string(bounded).unwrap();
    assert_eq!(
        serde_json::from_str::<AttemptQuote>(&old).unwrap(),
        request.quote
    );
    let meters = Meters::default();
    let wire = serde_json::to_value(&meters).unwrap();
    assert!(wire.get("unknownUsageAttempts").is_none());
    assert_eq!(serde_json::from_value::<Meters>(wire).unwrap(), meters);
    let (_, request) = fixture();
    let wire = serde_json::to_value(&request.quote).unwrap();
    assert_eq!(
        wire,
        json!({"recordOnly": {"providerId":"provider",
        "endpoint":"https://provider.example/v1", "model":"uncontracted-model", "operation":"Responses"}})
    );
    for field in ["maximum", "priceCovered", "contract", "unboundedTokens"] {
        let mut forged = wire.clone();
        forged[field] = json!(true);
        assert!(
            serde_json::from_value::<AttemptQuote>(forged).is_err(),
            "{field}"
        );
    }
    for field in ["validUntil", "maximumInputTokens", "maximumPrice"] {
        let mut forged = wire.clone();
        forged["recordOnly"][field] = json!(1);
        assert!(
            serde_json::from_value::<AttemptQuote>(forged).is_err(),
            "{field}"
        );
    }
    let mut forged = wire;
    forged["recordOnly"]["operation"] = json!("Embeddings");
    assert!(serde_json::from_value::<AttemptQuote>(forged).is_err());
}

#[test]
fn record_only_routes_reject_ambiguous_credentials_and_invalid_names() {
    for endpoint in [
        "",
        "ftp://provider.example",
        "https:///",
        "https://user:secret@provider.example",
        "https://provider.example?token=x",
        "https://provider.example#fragment",
        "https://provider.example/\npath",
        " https://provider.example",
        "https://provider.example/ space",
        "https:\\provider.example",
    ] {
        let quote = AttemptQuote::record_only("provider", endpoint, "model", Operation::Responses);
        assert!(quote.validate(100, false).is_err(), "{endpoint:?}");
    }
    for name in [
        "".to_owned(),
        "x".repeat(254),
        " padded".into(),
        "model\nname".into(),
    ] {
        assert!(
            AttemptQuote::record_only(
                &name,
                "https://provider.example",
                "model",
                Operation::Responses
            )
            .validate(1, false)
            .is_err()
        );
        assert!(
            AttemptQuote::record_only(
                "provider",
                "https://provider.example",
                &name,
                Operation::Responses
            )
            .validate(1, false)
            .is_err()
        );
    }
    let (_, request) = fixture();
    assert_eq!(
        request.quote.expires_at(100).unwrap(),
        100 + RESERVATION_TTL_SECONDS
    );
    assert!(request.quote.expires_at(i64::MAX).is_err());
    assert!(!request.quote.price_covered());
    assert_eq!(request.quote.maximum(), Amounts::default());
    assert!(request.quote.validate(100, true).is_err());
}

#[test]
fn record_only_finite_account_or_any_ancestor_axis_denies_reserve_and_begin() {
    for target in ["account", "root", "child"] {
        for limits in [
            Limits {
                tokens: Some(100),
                usd_micros: None,
            },
            Limits {
                tokens: None,
                usd_micros: Some(100),
            },
        ] {
            let (ledger, request) = fixture();
            let reserved = ledger.reserve(&request, 100).unwrap().next;
            for (mut current, begin) in [(ledger, false), (reserved, true)] {
                if target == "account" {
                    current.limits = limits;
                } else {
                    current.nodes.get_mut(target).unwrap().authority.limits = limits;
                }
                if begin {
                    assert!(current.begin_dispatch(&command(&request), 101).is_err());
                } else {
                    assert!(current.reserve(&request, 100).is_err());
                }
            }
        }
    }
}

#[test]
fn record_only_rechecks_authority_funding_and_expiry() {
    for target in ["account", "root", "child", "digest"] {
        let (ledger, request) = fixture();
        let reserved = ledger.reserve(&request, 100).unwrap().next;
        for (mut current, begin) in [(ledger, false), (reserved, true)] {
            match target {
                "account" => current.phase = AccountPhase::Closed,
                "digest" => {
                    current
                        .nodes
                        .get_mut("child")
                        .unwrap()
                        .authority
                        .authorization_digest = format!("sha256:{}", "b".repeat(64))
                }
                uid => current.nodes.get_mut(uid).unwrap().active = false,
            }
            if begin {
                assert!(current.begin_dispatch(&command(&request), 101).is_err());
            } else {
                assert!(current.reserve(&request, 100).is_err());
            }
        }
    }
    let (ledger, request) = fixture();
    let reserved = ledger.reserve(&request, 100).unwrap().next;
    assert!(matches!(
        reserved.begin_dispatch(&command(&request), 130),
        Err(BudgetError::Expired)
    ));
    let (mut builtin, mut request, mut grant) = builtin_ledger("bridge-orchestrator");
    grant.limits = Limits::default();
    builtin.limits = Limits::default();
    let node = builtin.nodes.get_mut("builtin-uid").unwrap();
    node.authority.limits = Limits::default();
    // Finite funding cannot be turned into an uncapped grant merely by deleting limits.
    node.authority.effective_authorization = serde_json::to_value(grant).unwrap();
    request.quote = fixture().1.quote;
    assert!(builtin.reserve(&request, 100).is_err());
}

#[test]
fn record_only_actual_usage_exceeds_guessed_bounds_and_is_charged_once() {
    let (ledger, request) = fixture();
    let current = inflight(ledger, &request);
    let settlement = Settlement {
        attempt: command(&request),
        usage: Some(large_usage()),
    };
    let result = restart(&current).settle(&settlement).unwrap();
    assert!(!result.value.uncertain);
    assert_eq!(result.value.charged.unwrap().tokens, 3_000_000);
    for meters in [
        &result.next.meters,
        &result.next.nodes["root"].meters,
        &result.next.nodes["child"].meters,
    ] {
        assert_eq!(meters.settled.tokens, 3_000_000);
        assert_eq!(meters.settled.usd_micros, 0);
        assert_eq!(meters.unpriced_attempts, 1);
        assert_eq!(meters.unknown_usage_attempts, 0);
    }
    let current = restart(&result.next);
    assert!(!current.settle(&settlement).unwrap().changed);
    assert!(current.reserve(&request, 102).is_err());
    assert!(current.begin_dispatch(&command(&request), 102).is_err());
    assert!(
        current
            .narrow_root_limits(Limits {
                tokens: Some(4_000_000),
                usd_micros: None
            })
            .is_ok()
    );
    assert!(
        current
            .narrow_root_limits(Limits {
                tokens: None,
                usd_micros: Some(100)
            })
            .is_err()
    );
}

#[test]
fn record_only_unknown_and_cancelled_work_resolves_late_even_after_closure() {
    for action in ["missing", "stalled", "session", "subtree", "account"] {
        let (ledger, request) = fixture();
        let current = inflight(ledger, &request);
        let unknown = match action {
            "missing" => {
                current
                    .settle(&Settlement {
                        attempt: command(&request),
                        usage: None,
                    })
                    .unwrap()
                    .next
            }
            "stalled" => current.commit_uncertain_before(130).unwrap().next,
            "session" => current.close_session("record-pod").unwrap().next,
            "subtree" => current.close_subtree("root").unwrap().next,
            _ => current.close_account().unwrap().next,
        };
        let unknown = restart(&unknown);
        for meters in [
            &unknown.meters,
            &unknown.nodes["root"].meters,
            &unknown.nodes["child"].meters,
        ] {
            assert_eq!(meters.unknown_usage_attempts, 1, "{action}");
            assert_eq!(meters.unpriced_attempts, 1);
            assert_eq!(meters.total().unwrap(), Amounts::default());
        }
        assert_eq!(
            unknown.attempts[&command(&request).key.storage_key()].phase,
            AttemptPhase::Uncertain
        );
        assert!(
            !unknown
                .settle(&Settlement {
                    attempt: command(&request),
                    usage: None
                })
                .unwrap()
                .changed
        );
        let mut forged = command(&request);
        forged.wire_digest = format!("sha256:{}", "b".repeat(64));
        assert!(
            unknown
                .settle(&Settlement {
                    attempt: forged,
                    usage: Some(large_usage())
                })
                .is_err()
        );
        let settlement = Settlement {
            attempt: command(&request),
            usage: Some(large_usage()),
        };
        let result = unknown.settle(&settlement).unwrap();
        assert!(!result.value.uncertain);
        let resolved = restart(&result.next);
        for meters in [
            &resolved.meters,
            &resolved.nodes["root"].meters,
            &resolved.nodes["child"].meters,
        ] {
            assert_eq!(meters.unknown_usage_attempts, 0);
            assert_eq!(meters.unpriced_attempts, 1);
            assert_eq!(meters.settled.tokens, 3_000_000);
        }
        assert!(!resolved.settle(&settlement).unwrap().changed);
        assert!(resolved.reserve(&request, 102).is_err());
    }
}

#[test]
fn record_only_unknown_and_inflight_cannot_be_narrowed_into_enforcement() {
    for stage in ["reserved", "inflight", "unknown"] {
        let (ledger, request) = fixture();
        let reserved = ledger.reserve(&request, 100).unwrap().next;
        let current = match stage {
            "reserved" => reserved,
            "inflight" => {
                reserved
                    .begin_dispatch(&command(&request), 101)
                    .unwrap()
                    .next
            }
            _ => {
                reserved
                    .begin_dispatch(&command(&request), 101)
                    .unwrap()
                    .next
                    .settle(&Settlement {
                        attempt: command(&request),
                        usage: None,
                    })
                    .unwrap()
                    .next
            }
        };
        for limits in [
            Limits {
                tokens: Some(10_000_000),
                usd_micros: None,
            },
            Limits {
                tokens: None,
                usd_micros: Some(100),
            },
        ] {
            assert!(current.narrow_root_limits(limits).is_err(), "{stage}");
            for uid in ["root", "child"] {
                let mut authority = current.nodes[uid].authority.clone();
                authority.limits = limits;
                assert!(
                    current.update_authority(authority).is_err(),
                    "{stage}: {uid}"
                );
            }
        }
    }
    let (ledger, request) = fixture();
    let expired = ledger
        .reserve(&request, 100)
        .unwrap()
        .next
        .expire_undispatched(130)
        .unwrap()
        .next;
    assert_eq!(expired.meters.unpriced_attempts, 0);
    assert_eq!(expired.meters.unknown_usage_attempts, 0);
    assert!(
        expired
            .narrow_root_limits(Limits {
                tokens: Some(100),
                usd_micros: None
            })
            .is_ok()
    );
}

#[test]
fn record_only_invalid_or_overflow_usage_retains_evidence_and_unknown_counters() {
    let mut invalid = large_usage();
    invalid.reasoning_output_tokens = invalid.output_tokens + 1;
    let mut overflow = large_usage();
    overflow.input_tokens = u64::MAX;
    for report in [invalid, overflow] {
        for late in [false, true] {
            let (ledger, request) = fixture();
            let current = inflight(ledger, &request);
            let current = if late {
                current.close_account().unwrap().next
            } else {
                current
            };
            let result = current
                .settle(&Settlement {
                    attempt: command(&request),
                    usage: Some(report.clone()),
                })
                .unwrap();
            assert!(result.value.uncertain && result.value.breach);
            assert_eq!(result.next.phase, AccountPhase::Frozen);
            assert_eq!(result.next.meters.unknown_usage_attempts, 1);
            let row = &result.next.attempts[&command(&request).key.storage_key()];
            assert_eq!(
                serde_json::from_str::<Usage>(row.observed_breach.as_ref().unwrap()).unwrap(),
                report
            );
            let resolved = restart(&result.next)
                .settle(&Settlement {
                    attempt: command(&request),
                    usage: Some(large_usage()),
                })
                .unwrap()
                .next;
            assert_eq!(resolved.meters.unknown_usage_attempts, 0);
            assert_eq!(resolved.meters.settled.tokens, 3_000_000);
            assert_eq!(resolved.phase, AccountPhase::Frozen);
            restart(&resolved);
        }
    }
    let (mut ledger, request) = fixture();
    ledger.meters.settled.tokens = MAX_LEDGER_INTEGER;
    let current = inflight(ledger, &request);
    let result = current
        .settle(&Settlement {
            attempt: command(&request),
            usage: Some(large_usage()),
        })
        .unwrap();
    assert!(result.value.breach && result.value.uncertain);
    assert_eq!(result.next.meters.settled.tokens, MAX_LEDGER_INTEGER);
    assert_eq!(result.next.meters.unknown_usage_attempts, 1);
    restart(&result.next);
}

#[test]
fn record_only_unknown_counter_must_match_retained_tombstones() {
    let (ledger, request) = fixture();
    let unknown = inflight(ledger, &request)
        .close_session("record-pod")
        .unwrap()
        .next;
    for target in ["account", "root", "child"] {
        let mut corrupt = unknown.clone();
        if target == "account" {
            corrupt.meters.unknown_usage_attempts = 0;
        } else {
            corrupt
                .nodes
                .get_mut(target)
                .unwrap()
                .meters
                .unknown_usage_attempts = 0;
        }
        assert!(corrupt.validate().is_err(), "{target}");
    }
    let mut corrupt = unknown;
    corrupt.attempts.clear();
    assert!(corrupt.validate().is_err());
}
