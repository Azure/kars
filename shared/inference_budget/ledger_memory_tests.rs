// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn uncapped() -> Ledger {
    let mut current = ledger(100);
    current.limits = Limits::default();
    current.nodes.get_mut("root").unwrap().authority.limits = Limits::default();
    let mut child = task("child", Some("root"), 0);
    child.limits = Limits::default();
    current.register_task(child).unwrap().next
}

#[test]
fn memory_service_accepts_reviewed_uncapped_team_and_mission_without_override() {
    for kind in [RootKind::KarsTeam, RootKind::KarsTask] {
        let mut current = uncapped();
        current.root.kind = kind;
        let before = current.clone();
        assert!(current.unbounded_tokens.is_none());
        for uid in ["root", "child"] {
            assert!(
                current
                    .allows_memory_service(&identity(uid, "pod"), 100)
                    .unwrap()
            );
            assert!(current.requires_enforcement(uid).unwrap());
        }
        assert_eq!(
            current, before,
            "Memory allowance does not alter accounting"
        );
    }
}

#[test]
fn memory_service_denies_each_finite_account_ancestor_and_child_axis() {
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
            let mut current = uncapped();
            if target == "account" {
                current.limits = limits;
            } else {
                current.nodes.get_mut(target).unwrap().authority.limits = limits;
            }
            assert!(
                !current
                    .allows_memory_service(&identity("child", "pod"), 100)
                    .unwrap(),
                "{target}: {limits:?}"
            );
        }
    }
}

#[test]
fn memory_service_rechecks_active_authority_and_identity() {
    for target in ["account", "root", "child"] {
        let mut current = uncapped();
        if target == "account" {
            current.phase = AccountPhase::Closed;
        } else {
            current.nodes.get_mut(target).unwrap().active = false;
        }
        assert!(
            current
                .allows_memory_service(&identity("child", "pod"), 100)
                .is_err()
        );
    }
    let current = uncapped();
    assert!(
        current
            .allows_memory_service(&identity("unknown", "pod"), 100)
            .is_err()
    );
    let mut forged = identity("child", "pod");
    forged.authorization_digest = format!("sha256:{}", "b".repeat(64));
    assert!(current.allows_memory_service(&forged, 100).is_err());
    forged = identity("child", "pod");
    forged.sandbox.namespace = "foreign".into();
    assert!(current.allows_memory_service(&forged, 100).is_err());
}
