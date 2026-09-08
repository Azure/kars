// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pure pod-spec helpers extracted from the reconciler: pod
//! security context, isolation-based scheduling, and the
//! egress-guard init-container command. Kept here to bound
//! `reconciler/mod.rs` size; all functions are side-effect-free.

use serde_json::json;

use crate::crd::SandboxConfig;

/// Build pod security context, conditionally including SELinux options and
/// choosing between RuntimeDefault and Localhost seccomp profiles.
/// For Kata (confidential), we use RuntimeDefault since the VM provides isolation.
pub(crate) fn build_pod_security_context(cfg: &SandboxConfig) -> serde_json::Value {
    // Standard and Confidential use RuntimeDefault seccomp:
    //   standard     — basic container isolation, kernel-default syscall filter
    //   confidential — Kata VM boundary is the isolation layer
    // Enhanced uses custom Localhost seccomp (kars-strict) for strict syscall allowlist
    let seccomp = if cfg.isolation == "confidential"
        || cfg.isolation == "standard"
        || cfg.seccomp_profile == "RuntimeDefault"
        || cfg.seccomp_profile.is_empty()
    {
        json!({ "type": "RuntimeDefault" })
    } else {
        json!({
            "type": "Localhost",
            "localhostProfile": format!("profiles/{}.json", cfg.seccomp_profile)
        })
    };

    let mut ctx = json!({
        "runAsNonRoot": cfg.run_as_non_root,
        "runAsUser": 1000,
        "runAsGroup": 1000,
        "fsGroup": 1000,
        "seccompProfile": seccomp
    });

    // Only set seLinuxOptions if a non-empty context is specified
    if !cfg.selinux_context.is_empty() {
        ctx.as_object_mut().unwrap().insert(
            "seLinuxOptions".into(),
            json!({ "type": cfg.selinux_context }),
        );
    }

    ctx
}

/// Returns (runtimeClassName, nodeSelector) based on the isolation level.
///   standard   → runc on clawpool, no custom seccomp
///   enhanced   → runc on clawpool + Localhost seccomp (kars-strict)
///   confidential → Kata VM isolation on katapool
pub(crate) fn isolation_scheduling(isolation: &str) -> (Option<&'static str>, &'static str) {
    match isolation {
        "confidential" => (Some("kata-vm-isolation"), "sandbox-kata"),
        _ => (None, "sandbox"), // standard + enhanced both on clawpool
    }
}

pub(crate) fn cluster_default_model() -> Option<String> {
    [
        "KARS_TASK_DEFAULT_MODEL",
        "AZURE_OPENAI_DEPLOYMENT",
        "DEFAULT_MODEL",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok())
    .map(|value| value.trim().to_string())
    .find(|value| !value.is_empty())
}

pub(crate) fn sandbox_node_selector_from(
    raw: &str,
    default_pool: &str,
) -> Result<serde_json::Value, String> {
    if raw.trim().is_empty() || raw.trim() == "{}" {
        return Ok(serde_json::json!({ "kars.azure.com/pool": default_pool }));
    }
    let selector: serde_json::Map<String, serde_json::Value> = serde_json::from_str(raw)
        .map_err(|error| format!("KARS_SANDBOX_NODE_SELECTOR_JSON is invalid JSON: {error}"))?;
    if selector.is_empty()
        || selector
            .iter()
            .any(|(key, value)| key.trim().is_empty() || !value.is_string())
    {
        return Err("KARS_SANDBOX_NODE_SELECTOR_JSON must be a non-empty string map".into());
    }
    Ok(serde_json::Value::Object(selector))
}

/// Build the egress-guard init-container command.
///
/// Every sandbox, including SRE, gets the full lockdown:
/// UID 1000 → loopback + DNS allowed, everything else dropped, with
/// :80/:443 NAT-redirected to the inference-router on :8444 for L7
/// policy + audit.
///
/// SRE diagnostics use a registered, filtered loopback HTTPS service instead
/// of direct Kubernetes access with an agent-held privileged credential.
pub(crate) fn build_egress_guard_command(_is_sre_sandbox: bool) -> String {
    let mut cmd = String::with_capacity(1024);
    // Filter chain (OUTPUT): UID 1000 → allow loopback + DNS +
    // established, then DROP. Same for every sandbox kind.
    cmd.push_str("iptables -A OUTPUT -m owner --uid-owner 1000 -o lo -j ACCEPT && ");
    cmd.push_str("iptables -A OUTPUT -m owner --uid-owner 1000 -p udp --dport 53 -j ACCEPT && ");
    cmd.push_str("iptables -A OUTPUT -m owner --uid-owner 1000 -p tcp --dport 53 -j ACCEPT && ");
    cmd.push_str(
        "iptables -A OUTPUT -m owner --uid-owner 1000 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT && "
    );

    cmd.push_str("iptables -A OUTPUT -m owner --uid-owner 1000 -j DROP && ");

    // NAT chain (OUTPUT):  :80/:443 → REDIRECT to :8444 (transparent
    // proxy in the inference-router sidecar).  Same for every sandbox.
    cmd.push_str(
        "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -p tcp --dport 80 -j REDIRECT --to-port 8444 && "
    );
    cmd.push_str(
        "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -p tcp --dport 443 -j REDIRECT --to-port 8444 && "
    );

    cmd.push_str("echo 'egress-guard: UID 1000 → transparent proxy on :8444 (learn + enforce)'");

    cmd
}

#[cfg(test)]
#[allow(clippy::module_inception)]
mod egress_guard_tests {
    use super::build_egress_guard_command;

    #[test]
    fn standard_sandbox_has_no_apiserver_bypass() {
        let cmd = build_egress_guard_command(false);
        assert!(!cmd.contains("KUBERNETES_SERVICE_HOST"));
        assert!(cmd.contains("REDIRECT --to-port 8444"));
        assert!(cmd.contains("(learn + enforce)"));
        assert!(!cmd.contains("apiserver bypass"));
    }

    #[test]
    fn sre_uses_the_same_network_lockdown_without_an_apiserver_bypass() {
        let cmd = build_egress_guard_command(true);
        assert_eq!(cmd, build_egress_guard_command(false));
        assert!(!cmd.contains("KUBERNETES_SERVICE_HOST"));
        assert!(!cmd.contains("-j RETURN"));
    }

    #[test]
    fn both_modes_keep_the_filter_chain_lockdown() {
        for is_sre in [false, true] {
            let cmd = build_egress_guard_command(is_sre);
            // The filter-chain DROP rule is the actual lockdown — must
            // never be removed by either mode.
            assert!(
                cmd.contains("-A OUTPUT -m owner --uid-owner 1000 -j DROP"),
                "filter-chain DROP missing for is_sre={is_sre}"
            );
        }
    }
}
