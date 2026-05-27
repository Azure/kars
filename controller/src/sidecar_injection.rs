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
            "readOnlyRootFilesystem": false,
            "capabilities": {"drop": ["ALL"]},
            "seccompProfile": {"type": "RuntimeDefault"}
        },
        "resources": {
            "requests": {"cpu": "50m", "memory": "96Mi"},
            "limits":   {"cpu": "500m", "memory": "256Mi"}
        },
        // ASP.NET Data Protection writes encryption keys to
        // `/app/keys` (see Microsoft.Identity.Web.Sidecar.Program::
        // ConfigureDataProtection). The image's distroless filesystem
        // has /app owned by root with 0755, so UID 1002 cannot mkdir
        // there. Mount a per-pod emptyDir so the sidecar can write
        // its DataProtection keys without needing root or a writable
        // root filesystem.
        "volumeMounts": [
            {"name": "auth-sidecar-keys", "mountPath": "/app/keys"}
        ],
        "readinessProbe": {
            // Use httpHeaders to override Host so Kestrel's
            // HostFiltering middleware (which only allows
            // `Host: localhost`) accepts the kubelet's probe. Without
            // this override the kubelet sends the pod IP as Host and
            // the sidecar returns 400 Bad Request, leaving the
            // container ready=false forever even though the auth
            // surface is fully functional.
            "httpGet": {
                "path": "/healthz",
                "port": "auth-sdk",
                "httpHeaders": [
                    {"name": "Host", "value": format!("localhost:{SIDECAR_PORT}")}
                ]
            },
            "initialDelaySeconds": 1,
            "periodSeconds": 5,
            "timeoutSeconds": 2,
            "failureThreshold": 3
        },
        "livenessProbe": {
            "httpGet": {
                "path": "/healthz",
                "port": "auth-sdk",
                "httpHeaders": [
                    {"name": "Host", "value": format!("localhost:{SIDECAR_PORT}")}
                ]
            },
            "initialDelaySeconds": 15,
            "periodSeconds": 30,
            "timeoutSeconds": 3,
            "failureThreshold": 5
        }
    })
}

/// Name of the emptyDir volume that backs the sidecar's `/app/keys`
/// DataProtection scratch space. Must be added to the pod spec's
/// `volumes` array when the sidecar is injected — see the call site
/// in `reconciler/mod.rs`.
pub const SIDECAR_KEYS_VOLUME: &str = "auth-sidecar-keys";

/// Build the emptyDir volume entry for the sidecar's keys mount.
/// Returns the JSON pod-spec volume; the caller appends to the pod's
/// `volumes` array right after pushing the sidecar container.
pub fn build_sidecar_keys_volume() -> Value {
    json!({"name": SIDECAR_KEYS_VOLUME, "emptyDir": {"medium": "Memory", "sizeLimit": "16Mi"}})
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
///
/// We use `localhost:8080` rather than `127.0.0.1:8080` because the
/// Microsoft Entra SDK sidecar's built-in `HostFiltering` middleware
/// allows ONLY `Host: localhost` (see Microsoft.Identity.Web.Sidecar
/// `Program.ConfigureHostFiltering`). Calls with `Host: 127.0.0.1`
/// are rejected with `400 Bad Request - Invalid Hostname`. K8s
/// always wires `localhost` → 127.0.0.1 in each pod's /etc/hosts so
/// the loopback semantics are identical.
pub fn build_router_sidecar_url_env() -> Value {
    json!({"name": "AUTH_SIDECAR_URL", "value": format!("http://localhost:{SIDECAR_PORT}")})
}

/// Egress-guard iptables rule fragments emitted as part of the
/// `egress-guard` init container's startup script when the sandbox is
/// in `agent-id` mode.
///
/// The init container already drops most egress; these rules ADD:
///
/// - openclaw (UID 1000) cannot reach `127.0.0.1:SIDECAR_PORT` —
///   prevents untrusted agent code from impersonating the router and
///   acquiring downstream tokens. Uses `-I` to insert BEFORE the
///   existing `--uid-owner 1000 -o lo -j ACCEPT` rule — otherwise
///   the loopback-allow would shadow this REJECT and the security
///   boundary would silently break (rubber-duck finding #5).
/// - inference-router (UID 1001) cannot reach `169.254.169.254` —
///   defence-in-depth; the router has no business reading IMDS in
///   agent-id mode (the sidecar handles it). `-A` is fine here because
///   no prior rule for UID 1001 exists.
/// - openclaw (UID 1000) is already blocked from IMDS (`169.254.169.254`)
///   by the pre-existing catch-all `DROP UID 1000` rule, so no extra
///   rule is required for that boundary.
/// - sidecar (UID 1002) is implicitly allowed: the OUTPUT chain
///   default policy is ACCEPT and we add no restrictive rule for
///   UID 1002.
///
/// Returned as a vec of `iptables` argument lists so the caller can
/// emit them into the init container's shell script. Each element is
/// a complete `iptables` invocation minus the binary name. The order
/// of the returned vec is the order in which they MUST be applied to
/// preserve the security boundary.
pub fn agent_id_egress_rules() -> Vec<Vec<&'static str>> {
    vec![
        // Insert at position 1 (before any existing UID 1000 ACCEPT rule).
        // Otherwise the pre-existing `-A OUTPUT --uid-owner 1000 -o lo
        // -j ACCEPT` rule matches first and the sidecar block never fires.
        vec![
            "-I", "OUTPUT", "1", "-m", "owner", "--uid-owner", "1000",
            "-d", "127.0.0.1", "-p", "tcp", "--dport", "8080",
            "-j", "REJECT", "--reject-with", "tcp-reset",
        ],
        // Defence-in-depth: router (UID 1001) must NOT reach IMDS in
        // agent-id mode — sidecar is the only authorised IMDS caller.
        // `-A` is correct because no prior --uid-owner 1001 rule exists.
        vec![
            "-A", "OUTPUT", "-m", "owner", "--uid-owner", "1001",
            "-d", "169.254.169.254", "-j", "REJECT", "--reject-with", "icmp-host-prohibited",
        ],
    ]
}

/// Compose the full egress-guard shell command — both the baseline
/// rules (always applied) and the agent-id additions when requested.
///
/// Owning the script generation here (rather than scattered `concat!`
/// macros in `reconciler/mod.rs`) makes the security boundary auditable
/// in one place and makes it impossible to add a new agent-id rule
/// without explicitly choosing its insertion point.
///
/// The returned string is suitable as the `args` value for an `sh -c`
/// init-container invocation. It is `&&`-chained so any iptables
/// failure aborts the init container (which causes pod-startup
/// failure — exactly what we want; a partially-applied egress policy
/// is worse than no policy because it suggests false confidence).
///
/// The ordering inside this function is load-bearing — see the
/// per-rule comments in [`agent_id_egress_rules`].
pub fn build_egress_guard_command(agent_id_mode: bool) -> String {
    let mut parts: Vec<String> = Vec::new();

    // ── Agent-id additions (when active) ──────────────────────────
    //
    // These MUST be emitted BEFORE the baseline rules because they
    // use `-I OUTPUT 1` to insert at the chain head. Emitting them
    // first ensures the chain order is REJECT-then-ACCEPT-loopback,
    // not the reverse (which would silently break the boundary).
    if agent_id_mode {
        for rule in agent_id_egress_rules() {
            parts.push(format!("iptables {}", rule.join(" ")));
        }
    }

    // ── Baseline rules (existing behaviour, kept verbatim) ────────
    //
    // Filter chain: allow localhost, DNS, established — drop everything
    // else for UID 1000. The catch-all `DROP UID 1000` also blocks
    // IMDS (169.254.169.254) for the agent container.
    parts.extend([
        "iptables -A OUTPUT -m owner --uid-owner 1000 -o lo -j ACCEPT".to_string(),
        "iptables -A OUTPUT -m owner --uid-owner 1000 -p udp --dport 53 -j ACCEPT".to_string(),
        "iptables -A OUTPUT -m owner --uid-owner 1000 -p tcp --dport 53 -j ACCEPT".to_string(),
        // Reply packets to inbound gateway connections (WebUX, Telegram).
        "iptables -A OUTPUT -m owner --uid-owner 1000 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT".to_string(),
        "iptables -A OUTPUT -m owner --uid-owner 1000 -j DROP".to_string(),
        // NAT chain: redirect HTTP/HTTPS from UID 1000 to the
        // transparent forward proxy at port 8444 (router, UID 1001).
        "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -p tcp --dport 80 -j REDIRECT --to-port 8444".to_string(),
        "iptables -t nat -A OUTPUT -m owner --uid-owner 1000 ! -o lo -p tcp --dport 443 -j REDIRECT --to-port 8444".to_string(),
    ]);

    // Trailing echo gives a useful log line in `kubectl logs <pod> -c egress-guard`.
    if agent_id_mode {
        parts.push(
            "echo 'egress-guard: agent-id mode — UID 1000 blocked from sidecar; UID 1001 blocked from IMDS'"
                .to_string(),
        );
    } else {
        parts.push(
            "echo 'egress-guard: UID 1000 → transparent proxy on :8444 (learn + enforce)'"
                .to_string(),
        );
    }

    parts.join(" && ")
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
        assert_eq!(env["value"], "http://localhost:8080");
    }

    #[test]
    fn egress_rules_use_insert_before_baseline_loopback_allow() {
        // Regression for rubber-duck finding #5: the sidecar-block rule
        // MUST be `-I OUTPUT 1` (insert at position 1) so it runs
        // BEFORE the pre-existing `-A OUTPUT --uid-owner 1000 -o lo
        // -j ACCEPT`. A naive `-A` here would mean the loopback allow
        // matches first and the agent silently has access to the
        // sidecar.
        let rules = agent_id_egress_rules();
        assert_eq!(rules.len(), 2, "agent-id mode emits exactly two iptables rules");

        let sidecar_block = &rules[0];
        let s = sidecar_block.join(" ");
        assert!(
            s.starts_with("-I OUTPUT 1 "),
            "sidecar-block must use -I OUTPUT 1 (insert at chain head); got: {s}"
        );
        assert!(s.contains("--uid-owner 1000"));
        assert!(s.contains("--dport 8080"));
        assert!(s.contains("REJECT"));

        // Router-IMDS block can use -A since no prior --uid-owner 1001 rule exists.
        let router_block = &rules[1];
        let r = router_block.join(" ");
        assert!(r.contains("--uid-owner 1001"));
        assert!(r.contains("169.254.169.254"));
        assert!(r.contains("REJECT"));
    }

    #[test]
    fn egress_guard_command_legacy_mode_matches_existing_behaviour() {
        // When agent_id_mode = false the output must contain exactly
        // the historical seven iptables lines plus the trailing echo.
        // Pinning this shape protects existing (non-agent-id) sandboxes
        // from any accidental regression.
        let cmd = build_egress_guard_command(false);
        assert!(cmd.contains("--uid-owner 1000 -o lo -j ACCEPT"));
        assert!(cmd.contains("--dport 53 -j ACCEPT"));
        assert!(cmd.contains("ctstate ESTABLISHED,RELATED -j ACCEPT"));
        assert!(cmd.contains("--uid-owner 1000 -j DROP"));
        assert!(cmd.contains("REDIRECT --to-port 8444"));
        assert!(cmd.contains("UID 1000 → transparent proxy on :8444"));
        // No agent-id additions.
        assert!(!cmd.contains("--dport 8080"));
        assert!(!cmd.contains("--uid-owner 1001"));
    }

    #[test]
    fn egress_guard_command_agent_id_mode_prepends_security_rules() {
        let cmd = build_egress_guard_command(true);
        // Security: the sidecar REJECT must appear before the loopback
        // ACCEPT in the script text — guarantees `-I` is meaningless
        // even if iptables semantics were ever mis-read by a reader.
        let block_pos = cmd
            .find("--uid-owner 1000 -d 127.0.0.1")
            .expect("sidecar-block rule present");
        let allow_pos = cmd
            .find("--uid-owner 1000 -o lo -j ACCEPT")
            .expect("loopback-allow rule present");
        assert!(
            block_pos < allow_pos,
            "sidecar block (idx {block_pos}) must precede loopback allow (idx {allow_pos}) in the egress-guard script"
        );
        // Both agent-id rules present.
        assert!(cmd.contains("--uid-owner 1000 -d 127.0.0.1 -p tcp --dport 8080 -j REJECT"));
        assert!(cmd.contains("--uid-owner 1001 -d 169.254.169.254"));
        // Baseline rules preserved.
        assert!(cmd.contains("REDIRECT --to-port 8444"));
        // Mode-specific echo.
        assert!(cmd.contains("agent-id mode"));
    }

    #[test]
    fn egress_guard_command_is_shell_safe_chained() {
        // Every step is && joined so a failing iptables aborts the
        // init container — a partially-applied policy is worse than
        // no policy (gives false confidence).
        let cmd = build_egress_guard_command(true);
        let steps: Vec<&str> = cmd.split(" && ").collect();
        // Each non-empty step is either an iptables invocation or the
        // trailing echo.
        for step in &steps {
            assert!(
                step.starts_with("iptables ") || step.starts_with("echo "),
                "unexpected step shape: {step}"
            );
        }
    }
}
