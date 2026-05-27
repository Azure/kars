// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Sidecar container injection for kars sandboxes operating in
//! `agent-id` mesh-auth mode.
//!
//! Pure-function helpers (no k8s I/O) that the sandbox reconciler
//! calls when assembling the pod spec. Kept in a separate module so
//! the integration into `reconciler/mod.rs` remains a thin
//! call-site addition rather than a sprawling pod-spec edit.
//!
//! ## What this module owns
//!
//! - The sidecar container spec (image, UID, env, security context,
//!   resources, mounts).
//! - The egress-guard iptables rule fragments that ensure the
//!   security boundary documented in
//!   `docs/architecture/entra-agent-id/01-runtime-token-flow.md`
//!   (openclaw UID 1000 must NOT reach IMDS or `127.0.0.1:8080`).
//! - The env vars that pin the agent-identity app id into the
//!   inference-router (so router code cannot accept caller-supplied
//!   `?AgentIdentity=` values).
//!
//! ## What this module does NOT own
//!
//! - Provisioning the agent identity via Graph (see
//!   `crate::agent_identity`).
//! - Materialising the auth ConfigMap (see
//!   `crate::auth_config_reconciler`).
//! - Mode resolution (`Auto` → `AgentId` vs `Anonymous`) — that lives
//!   in the sandbox reconciler where it has the cluster-wide context.

use serde_json::{Value, json};

/// Reserved UID for the Microsoft Entra SDK auth sidecar.
///
/// Distinct from openclaw (1000) and inference-router (1001) so the
/// egress-guard init container can carve iptables `--uid-owner` rules
/// per identity. Documented in
/// `docs/architecture/entra-agent-id/01-runtime-token-flow.md`.
pub const SIDECAR_UID: i64 = 1002;

/// Default Microsoft Entra SDK sidecar image, pinned to the GA
/// distroless build that was validated end-to-end during the POC.
///
/// Operators may override via the `KARS_SIDECAR_IMAGE` env var
/// surfaced through controller config. We treat this constant as the
/// last-resort fallback only.
pub const DEFAULT_SIDECAR_IMAGE: &str =
    "mcr.microsoft.com/entra-sdk/auth-sidecar:1.0.0-azurelinux3.0-distroless";

/// Port the sidecar listens on inside the pod. Always loopback-only;
/// the sidecar's host-filtering middleware refuses any other host.
pub const SIDECAR_PORT: u16 = 8080;

/// Name of the ConfigMap that holds the rendered sidecar env vars.
/// Mirrors `crate::auth_config_reconciler::SIDECAR_ENV_CONFIGMAP`. We
/// pin it here as a string constant (rather than re-importing) so
/// callers of this module aren't forced to depend on the reconciler.
pub const SIDECAR_ENV_CONFIGMAP_NAME: &str = "kars-auth-sidecar-env";

/// Env var name the inference-router reads to learn which agent
/// identity to pin in its sidecar requests.
///
/// The router MUST NOT accept caller-supplied `AgentIdentity` values —
/// it uses only this env var. The egress-guard iptables rules
/// additionally prevent the openclaw container from talking to the
/// sidecar directly, so untrusted agent code cannot bypass this
/// pinning even if it tries (per rubber-duck finding #1).
pub const PINNED_AGENT_IDENTITY_ENV: &str = "PINNED_AGENT_IDENTITY_APP_ID";

/// Build the sidecar container spec.
///
/// Returns a `serde_json::Value` shaped like a Kubernetes
/// `core/v1.Container`. The sandbox reconciler appends this to the
/// pod's `spec.containers` array when `meshAuth.mode == AgentId`.
///
/// Arguments:
/// - `image_override`: when `Some`, replaces [`DEFAULT_SIDECAR_IMAGE`].
///   Surfaced via cluster config so operators can pin to a registry
///   mirror or downgrade to an older known-good build.
/// - `image_pull_policy`: matches the policy applied to the openclaw
///   container so first-boot semantics are consistent across the pod.
pub fn build_sidecar_container(image_override: Option<&str>, image_pull_policy: &str) -> Value {
    let image = image_override.unwrap_or(DEFAULT_SIDECAR_IMAGE);

    json!({
        "name": "auth-sidecar",
        "image": image,
        "imagePullPolicy": image_pull_policy,
        "ports": [
            {"containerPort": SIDECAR_PORT, "name": "auth-sdk"}
        ],
        // All sidecar configuration is rendered into a single ConfigMap
        // by `auth_config_reconciler` and projected as env vars here.
        // We deliberately avoid baking values into the pod spec so a
        // KarsAuthConfig update is reflected on the next pod rollout
        // without a controller code change.
        "envFrom": [
            {
                "configMapRef": {
                    "name": SIDECAR_ENV_CONFIGMAP_NAME,
                    // optional=false on purpose: when agent-id mode is
                    // active, the sidecar must NOT silently start
                    // without its config. Pod stays in
                    // CreateContainerConfigError until the operator
                    // installs the CM.
                    "optional": false
                }
            }
        ],
        // ASP.NET Core wiring. Pinned identical to the documented
        // sidecar contract; never expose to non-localhost callers.
        "env": [
            {"name": "ASPNETCORE_URLS", "value": format!("http://127.0.0.1:{SIDECAR_PORT}")}
        ],
        "securityContext": {
            "runAsUser": SIDECAR_UID,
            "runAsNonRoot": true,
            "allowPrivilegeEscalation": false,
            "readOnlyRootFilesystem": false,  // .NET keys + token caches live in /app/keys
            "capabilities": {"drop": ["ALL"]},
            "seccompProfile": {"type": "RuntimeDefault"}
        },
        "resources": {
            "requests": {"cpu": "50m", "memory": "96Mi"},
            "limits":   {"cpu": "500m", "memory": "256Mi"}
        },
        "readinessProbe": {
            "httpGet": {"path": "/healthz", "port": "auth-sdk"},
            "initialDelaySeconds": 1,
            "periodSeconds": 5,
            "timeoutSeconds": 2,
            "failureThreshold": 3
        },
        "livenessProbe": {
            "httpGet": {"path": "/healthz", "port": "auth-sdk"},
            "initialDelaySeconds": 15,
            "periodSeconds": 30,
            "timeoutSeconds": 3,
            "failureThreshold": 5
        }
    })
}

/// Build the env-var entry pinning the agent identity app id into the
/// inference-router container.
///
/// The sandbox reconciler appends this to the router container's
/// `env` array when `meshAuth.mode == AgentId`. The router reads this
/// env var and refuses any caller-supplied `AgentIdentity` parameter,
/// guaranteeing that only the controller decides which Entra identity
/// the agent operates as (per rubber-duck finding #1).
pub fn build_router_pinned_identity_env(agent_app_id: &str) -> Value {
    json!({"name": PINNED_AGENT_IDENTITY_ENV, "value": agent_app_id})
}

/// Build the env-var entry that points the router at the sidecar URL.
/// Constant for now — operators don't get to override the loopback
/// port without recompiling, since the egress-guard rules pin it too.
pub fn build_router_sidecar_url_env() -> Value {
    json!({"name": "AUTH_SIDECAR_URL", "value": format!("http://127.0.0.1:{SIDECAR_PORT}")})
}

/// Egress-guard iptables rule fragments emitted as part of the
/// `egress-guard` init container's startup script when the sandbox is
/// in `agent-id` mode.
///
/// The init container already drops most egress; these rules ADD:
///
/// - openclaw (UID 1000) cannot reach `127.0.0.1:SIDECAR_PORT` —
///   prevents untrusted agent code from impersonating the router and
///   acquiring downstream tokens.
/// - openclaw (UID 1000) cannot reach `169.254.169.254` (IMDS) —
///   prevents untrusted agent code from pulling the controller MI's
///   raw token and impersonating the blueprint directly.
/// - inference-router (UID 1001) cannot reach `169.254.169.254` —
///   defence-in-depth; the router has no business reading IMDS in
///   agent-id mode (the sidecar handles it).
/// - sidecar (UID 1002) is allowed to reach IMDS and the Entra
///   token endpoint over the host network namespace.
///
/// Returned as a vec of `iptables` argument lists so the caller can
/// emit them into the init container's shell script. Each element is
/// a complete `iptables` invocation minus the binary name.
pub fn agent_id_egress_rules() -> Vec<Vec<&'static str>> {
    vec![
        // Block UID 1000 from sidecar.
        vec![
            "-A", "OUTPUT", "-m", "owner", "--uid-owner", "1000",
            "-d", "127.0.0.1", "-p", "tcp", "--dport", "8080",
            "-j", "REJECT", "--reject-with", "tcp-reset",
        ],
        // Block UID 1000 from IMDS.
        vec![
            "-A", "OUTPUT", "-m", "owner", "--uid-owner", "1000",
            "-d", "169.254.169.254", "-j", "REJECT", "--reject-with", "icmp-host-prohibited",
        ],
        // Block UID 1001 (router) from IMDS — sidecar mediates.
        vec![
            "-A", "OUTPUT", "-m", "owner", "--uid-owner", "1001",
            "-d", "169.254.169.254", "-j", "REJECT", "--reject-with", "icmp-host-prohibited",
        ],
        // Explicitly allow UID 1002 (sidecar) to reach IMDS.
        // Placed before any catch-all REJECT for the sidecar UID.
        vec![
            "-I", "OUTPUT", "-m", "owner", "--uid-owner", "1002",
            "-d", "169.254.169.254", "-j", "ACCEPT",
        ],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_container_pins_safe_uid_and_caps() {
        let c = build_sidecar_container(None, "IfNotPresent");
        assert_eq!(c["name"], "auth-sidecar");
        assert_eq!(c["securityContext"]["runAsUser"], SIDECAR_UID);
        assert_eq!(c["securityContext"]["runAsNonRoot"], true);
        assert_eq!(c["securityContext"]["allowPrivilegeEscalation"], false);
        let caps = c["securityContext"]["capabilities"]["drop"]
            .as_array()
            .expect("capabilities.drop");
        assert!(caps.iter().any(|v| v == "ALL"));
    }

    #[test]
    fn sidecar_default_image_when_no_override() {
        let c = build_sidecar_container(None, "IfNotPresent");
        assert_eq!(c["image"], DEFAULT_SIDECAR_IMAGE);
    }

    #[test]
    fn sidecar_image_override_takes_effect() {
        let c = build_sidecar_container(Some("private.registry/sidecar:1.0.0"), "Always");
        assert_eq!(c["image"], "private.registry/sidecar:1.0.0");
        assert_eq!(c["imagePullPolicy"], "Always");
    }

    #[test]
    fn sidecar_envfrom_points_at_required_configmap() {
        let c = build_sidecar_container(None, "IfNotPresent");
        let env_from = c["envFrom"].as_array().expect("envFrom");
        assert_eq!(env_from.len(), 1);
        let cm = &env_from[0]["configMapRef"];
        assert_eq!(cm["name"], SIDECAR_ENV_CONFIGMAP_NAME);
        assert_eq!(cm["optional"], false);
    }

    #[test]
    fn pinned_identity_env_has_correct_name_and_value() {
        let env = build_router_pinned_identity_env("agent-app-123");
        assert_eq!(env["name"], PINNED_AGENT_IDENTITY_ENV);
        assert_eq!(env["value"], "agent-app-123");
    }

    #[test]
    fn router_sidecar_url_env_points_at_loopback() {
        let env = build_router_sidecar_url_env();
        assert_eq!(env["name"], "AUTH_SIDECAR_URL");
        assert_eq!(env["value"], "http://127.0.0.1:8080");
    }

    #[test]
    fn egress_rules_cover_required_boundaries() {
        let rules = agent_id_egress_rules();
        // Four rules required for the security boundary in
        // docs/architecture/entra-agent-id/01-runtime-token-flow.md.
        assert_eq!(rules.len(), 4);

        // UID 1000 must not reach sidecar.
        assert!(rules.iter().any(|r| {
            let s = r.join(" ");
            s.contains("--uid-owner 1000") && s.contains("--dport 8080") && s.contains("REJECT")
        }));
        // UID 1000 must not reach IMDS.
        assert!(rules.iter().any(|r| {
            let s = r.join(" ");
            s.contains("--uid-owner 1000") && s.contains("169.254.169.254") && s.contains("REJECT")
        }));
        // UID 1001 (router) must not reach IMDS.
        assert!(rules.iter().any(|r| {
            let s = r.join(" ");
            s.contains("--uid-owner 1001") && s.contains("169.254.169.254") && s.contains("REJECT")
        }));
        // UID 1002 (sidecar) must explicitly be allowed to IMDS.
        assert!(rules.iter().any(|r| {
            let s = r.join(" ");
            s.contains("--uid-owner 1002") && s.contains("169.254.169.254") && s.contains("ACCEPT")
        }));
    }
}
