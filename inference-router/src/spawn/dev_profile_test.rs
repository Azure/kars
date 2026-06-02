// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Dev-profile sub-agent CRD shape test. Pulled out of `mod tests`
//! inside `spawn::mod` to keep that file under the §4.2 LOC budget.
//!
//! Pins both branches of `build_sub_agent_crd_with_labels`:
//! * `KARS_DEV_PROFILE` unset → strict prod default
//!   (`approvalRequired=true`, `egressMode=Strict`).
//! * `KARS_DEV_PROFILE=true` → relaxed dev default
//!   (`approvalRequired=false`, `egressMode=Learn`).

use super::*;
use std::collections::BTreeMap;

const DEV_ENV: &str = "KARS_DEV_PROFILE";

struct EnvGuard {
    name: &'static str,
    prior: Option<String>,
}
impl EnvGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let prior = std::env::var(name).ok();
        // SAFETY: this test calls `build_sub_agent_crd_with_labels`
        // synchronously; the guard scope brackets the single env read.
        unsafe { std::env::set_var(name, value) };
        Self { name, prior }
    }
    fn unset(name: &'static str) -> Self {
        let prior = std::env::var(name).ok();
        unsafe { std::env::remove_var(name) };
        Self { name, prior }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

fn req(agent_id: &str) -> SpawnRequest {
    SpawnRequest {
        agent_id: agent_id.into(),
        model: None,
        governance: true,
        trust_threshold: None,
        learn_egress: false,
        isolation: None,
        token_budget_daily: None,
        token_budget_per_request: None,
        trusted_peers: None,
        handoff: None,
    }
}

#[test]
fn sub_agent_crd_relaxes_network_policy_under_dev_profile() {
    let _off = EnvGuard::unset(DEV_ENV);
    let strict = build_sub_agent_crd_with_labels(
        "p",
        "kars-system",
        "enhanced",
        "gpt-5.4",
        &req("c"),
        &BTreeMap::new(),
    );
    assert_eq!(strict["spec"]["networkPolicy"]["approvalRequired"], true);
    assert_eq!(strict["spec"]["networkPolicy"]["egressMode"], "Strict");

    let _on = EnvGuard::set(DEV_ENV, "true");
    let relaxed = build_sub_agent_crd_with_labels(
        "p",
        "kars-system",
        "enhanced",
        "gpt-5.4",
        &req("c"),
        &BTreeMap::new(),
    );
    assert_eq!(relaxed["spec"]["networkPolicy"]["approvalRequired"], false);
    assert_eq!(relaxed["spec"]["networkPolicy"]["egressMode"], "Learn");
}
