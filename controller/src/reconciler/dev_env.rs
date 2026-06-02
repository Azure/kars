// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Dev-mode env-var injection — split out of `reconciler::mod` to keep
//! that file under the §4.2 LOC budget. Inlined here is the surgical
//! propagation of two signals into every reconciled sandbox:
//!
//! * `KARS_PROVIDER` — set when the controller itself runs with a dev
//!   creds bundle (Helm `controller.extraEnv` referencing
//!   `kars-dev-creds`). Pushed onto BOTH the router and the openclaw
//!   container so the OpenClaw plugin's Foundry-tool gate
//!   (`runtimes/openclaw/src/index.ts`) fires correctly in
//!   github-copilot / github-models modes.
//! * `KARS_DEV_PROFILE=true` — set only in `kars dev` (docker /
//!   local-k8s). Triggers relaxed sub-agent CRD defaults
//!   (`egressMode=Learn`, `approvalRequired=false`) inside the router
//!   spawn helper, plus the three governance-noise suppressors the
//!   docker dev parent already enjoys.
//!
//! Production AKS leaves both env vars unset and the strict defaults
//! stand.

use serde_json::json;

pub(super) fn apply(
    dev_provider: &str,
    dev_profile: bool,
    is_openclaw: bool,
    router_env: &mut Vec<serde_json::Value>,
    openclaw_env: &mut Vec<serde_json::Value>,
) {
    if !dev_provider.is_empty() {
        router_env.push(json!({"name": "KARS_PROVIDER", "value": dev_provider}));
        if is_openclaw {
            openclaw_env.push(json!({"name": "KARS_PROVIDER", "value": dev_provider}));
        }
    }
    if dev_profile {
        for (k, v) in [
            ("KARS_DEV_PROFILE", "true"),
            ("KARS_SUPPRESS_EXFIL_URL", "1"),
            ("KARS_SUPPRESS_CONTENT_FLAGS", "violence"),
            ("KARS_CONTENT_FLAG_MIN_SEVERITY", "medium"),
        ] {
            router_env.push(json!({"name": k, "value": v}));
        }
        if is_openclaw {
            openclaw_env.push(json!({"name": "KARS_DEV_PROFILE", "value": "true"}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_names(v: &[serde_json::Value]) -> Vec<&str> {
        v.iter().map(|e| e["name"].as_str().unwrap()).collect()
    }

    #[test]
    fn no_provider_no_profile_pushes_nothing() {
        let (mut r, mut o) = (vec![], vec![]);
        apply("", false, true, &mut r, &mut o);
        assert!(r.is_empty() && o.is_empty());
    }

    #[test]
    fn provider_pushed_to_both_containers_when_openclaw() {
        let (mut r, mut o) = (vec![], vec![]);
        apply("github-copilot", false, true, &mut r, &mut o);
        assert_eq!(env_names(&r), vec!["KARS_PROVIDER"]);
        assert_eq!(env_names(&o), vec!["KARS_PROVIDER"]);
    }

    #[test]
    fn provider_not_pushed_to_openclaw_when_not_openclaw_runtime() {
        let (mut r, mut o) = (vec![], vec![]);
        apply("github-copilot", false, false, &mut r, &mut o);
        assert_eq!(env_names(&r), vec!["KARS_PROVIDER"]);
        assert!(o.is_empty());
    }

    #[test]
    fn dev_profile_pushes_relaxations_to_router_and_marker_to_openclaw() {
        let (mut r, mut o) = (vec![], vec![]);
        apply("", true, true, &mut r, &mut o);
        assert_eq!(
            env_names(&r),
            vec![
                "KARS_DEV_PROFILE",
                "KARS_SUPPRESS_EXFIL_URL",
                "KARS_SUPPRESS_CONTENT_FLAGS",
                "KARS_CONTENT_FLAG_MIN_SEVERITY"
            ]
        );
        assert_eq!(env_names(&o), vec!["KARS_DEV_PROFILE"]);
    }
}
