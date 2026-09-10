// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Observations only. Admission continues to validate the sealed UID-bound
//! ledger; editing a phase or Ready condition cannot change a grant or balance.

use super::{
    account::{AccountStatusPhase as Phase, BOOTSTRAP, KarsBudgetAccount, KarsBudgetAccountStatus},
    store::{Store, StoreError},
};
use crate::{
    inference_budget_contract::{AccountPhase, Amounts, Limits, MAX_ATTEMPTS},
    status::conditions,
};
use kube::ResourceExt;

struct Observation {
    phase: Phase,
    ready: &'static str,
    valid: &'static str,
    reason: &'static str,
    message: &'static str,
}

fn blocked(phase: Phase, reason: &'static str, message: &'static str) -> Observation {
    Observation {
        phase,
        ready: "False",
        valid: "True",
        reason,
        message,
    }
}

fn at_limit(limits: Limits, amount: Amounts) -> bool {
    let limits = limits.normalized();
    limits.tokens.is_some_and(|cap| amount.tokens >= cap)
        || limits
            .usd_micros
            .is_some_and(|cap| amount.usd_micros >= cap)
}

fn observe(account: &KarsBudgetAccount, error: Option<&StoreError>) -> Observation {
    if account.annotations().get(BOOTSTRAP).map(String::as_str) == Some("pending")
        && Store::bootstrap_ledger(account).is_ok()
    {
        return Observation {
            phase: Phase::Bootstrap,
            ready: "False",
            valid: "Unknown",
            reason: "BootstrapPending",
            message: "The account is not sealed; no governed-inference dispatch is authorized.",
        };
    }
    let Ok(ledger) = Store::ledger(account) else {
        return Observation {
            phase: Phase::Corrupt,
            ready: "False",
            valid: "False",
            reason: "LedgerInvalid",
            message: "The sealed account identity or ledger is invalid; balances are retained and admission fails closed.",
        };
    };
    match ledger.phase {
        AccountPhase::Frozen => {
            return blocked(
                Phase::Frozen,
                "ContractBreach",
                "Provider usage breached the governed-inference bound; the account is frozen.",
            );
        }
        AccountPhase::Closing => {
            return blocked(
                Phase::Closing,
                "AccountClosing",
                "Account authority is closing; outstanding governed-inference liabilities remain funded.",
            );
        }
        AccountPhase::Closed => {
            return blocked(
                Phase::Closed,
                "AccountRetired",
                "Account authority is retired; historical governed-inference charges are retained.",
            );
        }
        AccountPhase::Active => {}
    }
    if let Some(error) = error {
        return match error {
            StoreError::Api { .. } | StoreError::Contention => Observation {
                phase: Phase::Unknown,
                ready: "Unknown",
                valid: "True",
                reason: "ReconciliationUnavailable",
                message: "Live authority reconciliation could not complete; recorded liabilities are retained, not re-authorized.",
            },
            _ => blocked(
                Phase::Blocked,
                "ReconciliationBlocked",
                "Live authority reconciliation failed closed; recorded liabilities are retained.",
            ),
        };
    }
    let durable = ledger.meters.settled.checked_add(ledger.meters.uncertain);
    if durable.is_ok_and(|amount| at_limit(ledger.limits, amount)) {
        return blocked(
            Phase::Blocked,
            "BudgetExhausted",
            "Settled or uncertain governed-inference charges have exhausted a declared account ceiling.",
        );
    }
    if ledger
        .meters
        .total()
        .is_ok_and(|amount| at_limit(ledger.limits, amount))
    {
        return blocked(
            Phase::Blocked,
            "BudgetReserved",
            "Outstanding reservations occupy a declared ceiling; already funded work is retained.",
        );
    }
    if !ledger.nodes.is_empty() && ledger.nodes.values().all(|node| !node.active) {
        return blocked(
            Phase::Blocked,
            "AuthorityRevoked",
            "All enrolled task authorities are closed; reopening requires controller-verified authority.",
        );
    }
    if ledger.attempts.len() >= MAX_ATTEMPTS {
        return blocked(
            Phase::Blocked,
            "AttemptCapacityReached",
            "The bounded attempt ledger is full; replay fences and funded work are retained.",
        );
    }
    Observation {
        phase: Phase::Active,
        ready: "True",
        valid: "True",
        reason: "LedgerAvailable",
        message: "The sealed governed-inference ledger has admission headroom; each request still requires live authority and a funded contract.",
    }
}

pub(super) fn project(
    account: &KarsBudgetAccount,
    error: Option<&StoreError>,
) -> KarsBudgetAccountStatus {
    let observation = observe(account, error);
    let mut status = account.status.clone().unwrap_or_default();
    let prior = &status.conditions;
    status.conditions = [
        (conditions::TYPE_READY, observation.ready),
        ("LedgerValid", observation.valid),
    ]
    .into_iter()
    .map(|(kind, value)| {
        conditions::preserve_transition_time(
            conditions::find(prior, kind),
            kind,
            value,
            observation.reason,
            observation.message,
            account.metadata.generation,
        )
    })
    .collect();
    status.phase = Some(observation.phase);
    status.observed_generation = account.metadata.generation;
    status
}
