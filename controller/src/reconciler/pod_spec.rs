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
/// Standard sandboxes (every kind except SRE) get the full lockdown:
/// UID 1000 → loopback + DNS allowed, everything else dropped, with
/// :80/:443 NAT-redirected to the inference-router on :8444 for L7
/// policy + audit.
///
/// SRE-mode sandboxes (labelled `kars.azure.com/role=sre`) get ONE
/// extra rule inserted into the OUTPUT NAT chain BEFORE the generic
/// REDIRECT:  apiserver-bound traffic (KUBERNETES_SERVICE_HOST :
/// KUBERNETES_SERVICE_PORT_HTTPS, both kubelet-auto-injected envs)
/// is RETURNed — i.e. NOT NAT'd to :8444 — so the SRE plugin's K8s
/// API client (sre_kube.py) can hit the apiserver directly with its
/// projected SA token.
///
/// The K8s audit log is the audit surface for these apiserver calls
/// (the router's L7 audit doesn't capture them, but K8s audit is
/// stronger — every call carries the SA identity and the verb).
///
/// Privilege-containment design:  this capability is uniquely held by
/// the SRE sandbox per the proposal §7.8. Future Slice 3 will add
/// ValidatingAdmissionPolicies to gate WHO can apply the
/// `role=sre` label (only chart-installer SAs; see §7.8.10 design).
pub(crate) fn build_egress_guard_command(is_sre_sandbox: bool) -> String {
    let mut cmd = String::with_capacity(1024);
    // Filter chain (OUTPUT): UID 1000 → allow loopback + DNS +
    // established, then DROP. Same for every sandbox kind.
    cmd.push_str("iptables -A OUTPUT -m owner --uid-owner 1000 -o lo -j ACCEPT && ");
    cmd.push_str("iptables -A OUTPUT -m owner --uid-owner 1000 -p udp --dport 53 -j ACCEPT && ");
    cmd.push_str("iptables -A OUTPUT -m owner --uid-owner 1000 -p tcp --dport 53 -j ACCEPT && ");
    cmd.push_str(
        "iptables -A OUTPUT -m owner --uid-owner 1000 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT && "
    );

    // SRE-mode-only: filter-chain ACCEPT for apiserver-bound traffic.
    // The filter chain runs AFTER the NAT chain — the NAT-bypass RETURN
    // below just decides "don't redirect", but the filter chain's DROP
    // (next rule) would still kill the packet. We have to ACCEPT it
    // here BEFORE the catch-all DROP.
    if is_sre_sandbox {
        cmd.push_str(
            "iptables -A OUTPUT -m owner --uid-owner 1000 \
             -d \"${KUBERNETES_SERVICE_HOST}\" \
             -p tcp --dport \"${KUBERNETES_SERVICE_PORT_HTTPS:-443}\" \
             -j ACCEPT && ",
        );
    }

    cmd.push_str("iptables -A OUTPUT -m owner --uid-owner 1000 -j DROP && ");

    // SRE-mode-only:  NAT-chain apiserver bypass.  Inserted BEFORE the
    // generic :443 REDIRECT so apiserver traffic short-circuits to the
    // real upstream rather than the router. KUBERNETES_SERVICE_HOST
    // and KUBERNETES_SERVICE_PORT_HTTPS are auto-injected by the
    // kubelet on every container (including init containers).
    if is_sre_sandbox {
        cmd.push_str(
            "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 \
             -d \"${KUBERNETES_SERVICE_HOST}\" \
             -p tcp --dport \"${KUBERNETES_SERVICE_PORT_HTTPS:-443}\" \
             -j RETURN && ",
        );
    }

    // NAT chain (OUTPUT):  :80/:443 → REDIRECT to :8444 (transparent
    // proxy in the inference-router sidecar).  Same for every sandbox.
    cmd.push_str(
        "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -p tcp --dport 80 -j REDIRECT --to-port 8444 && "
    );
    cmd.push_str(
        "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -p tcp --dport 443 -j REDIRECT --to-port 8444 && "
    );

    if is_sre_sandbox {
        cmd.push_str(
            "echo 'egress-guard: UID 1000 → transparent proxy on :8444 + apiserver bypass (SRE mode)'"
        );
    } else {
        cmd.push_str(
            "echo 'egress-guard: UID 1000 → transparent proxy on :8444 (learn + enforce)'",
        );
    }

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
    fn sre_sandbox_inserts_apiserver_bypass_before_redirect() {
        let cmd = build_egress_guard_command(true);
        // The bypass MUST come before the :443 REDIRECT — otherwise
        // the REDIRECT wins (iptables -A appends; rules evaluate in
        // order) and the bypass is dead code.
        let bypass_pos = cmd
            .find("-t nat -A OUTPUT -m owner --uid-owner 1000              -d \"${KUBERNETES_SERVICE_HOST}\"")
            .or_else(|| cmd.find("-t nat -A OUTPUT -m owner --uid-owner 1000 \t\t\t -d \"${KUBERNETES_SERVICE_HOST}\""))
            .or_else(|| {
                // Match the NAT-chain bypass specifically (not the filter ACCEPT)
                cmd.match_indices("-t nat -A OUTPUT")
                    .find(|(i, _)| cmd[*i..].contains("KUBERNETES_SERVICE_HOST"))
                    .map(|(i, _)| i)
            })
            .expect("NAT-chain bypass rule missing");
        let redirect_pos = cmd
            .find("--dport 443 -j REDIRECT")
            .expect("redirect rule missing");
        assert!(
            bypass_pos < redirect_pos,
            "NAT bypass at {bypass_pos} must precede redirect at {redirect_pos}"
        );
        assert!(cmd.contains("apiserver bypass (SRE mode)"));

        // ALSO check the filter-chain ACCEPT exists BEFORE the DROP — this
        // was the bug we hit live: NAT bypass alone wasn't enough because
        // the filter chain's DROP for UID 1000 killed the packet anyway.
        let filter_accept = cmd
            .find(
                "-A OUTPUT -m owner --uid-owner 1000              -d \"${KUBERNETES_SERVICE_HOST}\"",
            )
            .or_else(|| {
                cmd.match_indices("-A OUTPUT -m owner --uid-owner 1000")
                    .find(|(i, _)| {
                        let tail = &cmd[*i..*i + 200.min(cmd.len() - *i)];
                        tail.contains("KUBERNETES_SERVICE_HOST") && tail.contains("-j ACCEPT")
                    })
                    .map(|(i, _)| i)
            })
            .expect("filter-chain ACCEPT for apiserver missing");
        let filter_drop = cmd
            .find("-A OUTPUT -m owner --uid-owner 1000 -j DROP")
            .expect("filter DROP rule missing");
        assert!(
            filter_accept < filter_drop,
            "filter ACCEPT at {filter_accept} must precede DROP at {filter_drop}"
        );
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
