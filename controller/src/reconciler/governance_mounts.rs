// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Cross-namespace governance artifact mirroring + pod mount injection.
//!
//! Phase 3 S7 wiring: closes the gap where governance CRD reconcilers
//! (`tool_policy_reconciler`, `mcp_server_reconciler`, `a2a_agent_reconciler`)
//! produce ConfigMaps/Secrets in the **user's** namespace but the sandbox
//! pod runs in `azureclaw-{name}` and cannot mount cross-namespace.
//!
//! ## Strategy
//!
//! For each governance CR referenced by a `ClawSandbox`:
//!
//! 1. Look up the source artifact (ConfigMap/Secret) in the user's
//!    namespace (`sandbox_self_ns`).
//! 2. Copy its `data` / `stringData` / `binaryData` into a new
//!    sandbox-local artifact (same name) in the sandbox namespace
//!    (`azureclaw-{name}`), labelled with provenance.
//! 3. Mount the sandbox-local copy into the inference-router container.
//!
//! The mirror is one-way (controller-managed); operators do not edit the
//! sandbox-namespace copy directly. The reconcile loop refreshes the
//! mirror on every pass, so spec changes to the source CR propagate
//! within the standard reconcile cadence.
//!
//! ## Failure semantics
//!
//! - Source not found → mirror is **skipped**, mount is **omitted**, and
//!   the reconciler emits a warning event. The sandbox pod still starts;
//!   the router falls back to the no-CR default (e.g. empty policy
//!   engine, no JWKS, no AgentCard served).
//! - Source malformed → same behavior; logged at `warn`.
//! - Mirror write conflict → propagated up; reconciler requeues.
//!
//! This intentionally biases toward **availability** over strict
//! enforcement: a user mistyping `mcpServerRef.name` should not crash
//! the sandbox pod. The inference-router surfaces "no policy loaded" /
//! "JWKS missing" via its own metrics + status endpoints.

use anyhow::Result;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::{
    Api, Client,
    api::{Patch, PatchParams},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Kubernetes label values are limited to 63 bytes (RFC 1123). The owner
/// label value we want to emit (`<kind>.<name>`) can exceed 63 bytes when
/// the mirrored resource has a long name (e.g. an InferencePolicy whose
/// own ConfigMap name is already prefixed with `inferencepolicy-`).
///
/// We collapse over-long values to `<truncated-prefix>-<8-hex-digest>`
/// so the label stays unique-per-source and recognisable in `kubectl get
/// -l`, without violating the kubelet validator. Nothing reads this
/// label programmatically (it is a write-only provenance marker —
/// authoritative kind/namespace is in the sibling annotations) so
/// truncation is safe.
fn safe_label_value(raw: &str) -> String {
    const MAX: usize = 63;
    if raw.len() <= MAX {
        return raw.to_string();
    }
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    let suffix = format!(
        "-{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    );
    let head_len = MAX - suffix.len();
    let head = &raw[..head_len.min(raw.len())];
    format!("{head}{suffix}")
}

/// Owned-resource label key, attached to every mirror by the reconciler
/// so cleanup/observability can find them. Value format is
/// `<kind>.<name>` (lowercased) — note `.` not `/`: K8s label *values*
/// cannot contain `/` (only label *keys* may use the `prefix/name` form).
pub(crate) const MIRROR_OWNER_LABEL: &str = "azureclaw.azure.com/mirrored-from";
/// Sandbox name label, links the mirror back to its `ClawSandbox`.
pub(crate) const MIRROR_SANDBOX_LABEL: &str = "azureclaw.azure.com/sandbox";
/// Source-namespace annotation, records where the artifact originated.
pub(crate) const MIRROR_SOURCE_NS_ANNOTATION: &str = "azureclaw.azure.com/mirrored-from-namespace";
/// Source-kind annotation (`ToolPolicy` / `McpServer` / `A2AAgent`).
pub(crate) const MIRROR_SOURCE_KIND_ANNOTATION: &str = "azureclaw.azure.com/mirrored-from-kind";

/// Mount paths exposed inside the inference-router container.
pub mod paths {
    /// ToolPolicy compiled profile (JSON). Loaded by the AGT policy
    /// engine on startup and on hot-reload.
    pub const TOOL_POLICY_DIR: &str = "/etc/agt/policies";
    /// InferencePolicy compiled profile (JSON). Loaded by the
    /// inference-router's `inference_policy_loader` on startup; the
    /// digest is echoed via `/internal/policy-status` so the
    /// controller can close the §3 Ready ⇔ router-echo loop. Must
    /// match the router-side default in
    /// `inference-router/src/inference_policy_loader.rs::INFERENCE_POLICY_DIR_DEFAULT`.
    pub const INFERENCE_POLICY_DIR: &str = "/etc/azureclaw/inference";
    /// `ClawMemory` compiled binding (JSON). Loaded by the
    /// inference-router's `memory_binding_loader` on startup; the
    /// digest is echoed via `/internal/policy-status` so the
    /// controller can close the §3 Ready ⇔ router-echo loop
    /// (Slice 3a). Must match the router-side default in
    /// `inference-router/src/memory_binding_loader.rs::MEMORY_BINDING_DIR_DEFAULT`.
    pub const MEMORY_BINDING_DIR: &str = "/etc/azureclaw/memory";
    /// Signed egress allowlist (JSON). Compiled by the controller
    /// from the verified `spec.networkPolicy.allowlistRef` (or inline
    /// `allowedEndpoints` when no ref is set); loaded by the
    /// inference-router's `egress_allowlist_loader` which seeds the
    /// L7 hostname filter (`Blocklist.allowlist`). Digest is echoed
    /// via `/internal/policy-status` under `PolicyKind::EgressAllowlist`
    /// to close the §3 Ready ⇔ router-echo loop (Slice 5c.1). Must
    /// match the router-side default in
    /// `inference-router/src/egress_allowlist_loader.rs::EGRESS_ALLOWLIST_DIR_DEFAULT`.
    pub const EGRESS_ALLOWLIST_DIR: &str = "/etc/azureclaw/egress";
    /// Per-sandbox `EgressApproval` files (one `approval-{name}.json`
    /// per CR pointing at this sandbox). Owned by the
    /// `egress_approval_reconciler`; consumed by the inference-router's
    /// `egress_allowlist_loader` which UNIONs them with the baseline
    /// allowlist and echoes the merged-set digest via
    /// `/internal/policy-status` under `PolicyKind::EgressApproval`.
    /// Mounted with `optional: true` so sandbox pods start before any
    /// approval has landed. Must match the router-side default in
    /// `inference-router/src/egress_allowlist_loader.rs`
    /// (`EGRESS_APPROVAL_DIR_DEFAULT` in
    /// `inference-router/src/egress_allowlist_loader.rs`).
    pub const EGRESS_APPROVAL_DIR: &str = "/etc/azureclaw/egress-approvals";
    /// McpServer JWKS (used by the customer-MCP OAuth verifier).
    pub const MCP_JWKS_DIR: &str = "/etc/azureclaw/mcp";
    /// A2AAgent compiled signed AgentCard.
    pub const A2A_CARD_DIR: &str = "/etc/azureclaw/a2a-card";
    /// McpServer Ed25519 signing keypair Secret mount.
    pub const MCP_SIGNING_DIR: &str = "/etc/azureclaw/mcp-signing";
}

/// Result of mirroring a single resource. `Skipped` carries a reason
/// suitable for emitting as a Kubernetes Warning event without leaking
/// internal details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorOutcome {
    Mirrored,
    Skipped(String),
}

/// Mirror a ConfigMap from `src_ns/name` to `dst_ns/name`.
///
/// The destination ConfigMap is fully owned and managed by this
/// reconciler — operators must not edit it directly. The reconciler
/// applies via SSA with the [`crate::field_managers::CLAWSANDBOX`]
/// field manager.
pub async fn mirror_configmap(
    client: &Client,
    name: &str,
    src_ns: &str,
    dst_ns: &str,
    sandbox_name: &str,
    source_kind: &str,
) -> Result<MirrorOutcome, kube::Error> {
    let src_api: Api<ConfigMap> = Api::namespaced(client.clone(), src_ns);
    let src = match src_api.get(name).await {
        Ok(cm) => cm,
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            return Ok(MirrorOutcome::Skipped(format!(
                "source ConfigMap `{name}` not found in namespace `{src_ns}` \
                 — referenced governance CR may not have been reconciled yet"
            )));
        }
        Err(e) => return Err(e),
    };

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert(
        MIRROR_OWNER_LABEL.into(),
        safe_label_value(&format!("{source_kind}.{name}").to_lowercase()),
    );
    labels.insert(MIRROR_SANDBOX_LABEL.into(), sandbox_name.into());
    labels.insert(
        "app.kubernetes.io/managed-by".into(),
        "azureclaw-controller".into(),
    );

    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(MIRROR_SOURCE_NS_ANNOTATION.into(), src_ns.into());
    annotations.insert(MIRROR_SOURCE_KIND_ANNOTATION.into(), source_kind.into());

    let mut patch = json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": name,
            "namespace": dst_ns,
            "labels": labels,
            "annotations": annotations,
        }
    });
    if let Some(d) = src.data.as_ref() {
        patch
            .as_object_mut()
            .unwrap()
            .insert("data".into(), json!(d));
    }
    if let Some(b) = src.binary_data.as_ref() {
        patch
            .as_object_mut()
            .unwrap()
            .insert("binaryData".into(), json!(b));
    }

    let dst_api: Api<ConfigMap> = Api::namespaced(client.clone(), dst_ns);
    let pp = PatchParams::apply(crate::field_managers::CLAWSANDBOX).force();
    dst_api.patch(name, &pp, &Patch::Apply(patch)).await?;

    Ok(MirrorOutcome::Mirrored)
}

/// Mirror a Secret from `src_ns/name` to `dst_ns/name`.
///
/// **Security note.** Mirroring a Secret cross-namespace expands its
/// blast radius. We do this only for purpose-built governance Secrets
/// (`mcp-{name}-signing`) where the sandbox-namespace ServiceAccount
/// already has implicit access via its mount. The mirror destination
/// inherits the source's `type` (e.g., `Opaque`, `kubernetes.io/tls`).
pub async fn mirror_secret(
    client: &Client,
    name: &str,
    src_ns: &str,
    dst_ns: &str,
    sandbox_name: &str,
    source_kind: &str,
) -> Result<MirrorOutcome, kube::Error> {
    let src_api: Api<Secret> = Api::namespaced(client.clone(), src_ns);
    let src = match src_api.get(name).await {
        Ok(s) => s,
        Err(kube::Error::Api(ae)) if ae.code == 404 => {
            return Ok(MirrorOutcome::Skipped(format!(
                "source Secret `{name}` not found in namespace `{src_ns}`"
            )));
        }
        Err(e) => return Err(e),
    };

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert(
        MIRROR_OWNER_LABEL.into(),
        safe_label_value(&format!("{source_kind}.{name}").to_lowercase()),
    );
    labels.insert(MIRROR_SANDBOX_LABEL.into(), sandbox_name.into());
    labels.insert(
        "app.kubernetes.io/managed-by".into(),
        "azureclaw-controller".into(),
    );

    let mut annotations: BTreeMap<String, String> = BTreeMap::new();
    annotations.insert(MIRROR_SOURCE_NS_ANNOTATION.into(), src_ns.into());
    annotations.insert(MIRROR_SOURCE_KIND_ANNOTATION.into(), source_kind.into());

    let mut patch = json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": dst_ns,
            "labels": labels,
            "annotations": annotations,
        },
        "type": src.type_.as_deref().unwrap_or("Opaque"),
    });
    if let Some(d) = src.data.as_ref() {
        // k8s_openapi serialises ByteString as base64 already; pass through.
        patch
            .as_object_mut()
            .unwrap()
            .insert("data".into(), json!(d));
    }
    if let Some(s) = src.string_data.as_ref() {
        patch
            .as_object_mut()
            .unwrap()
            .insert("stringData".into(), json!(s));
    }

    let dst_api: Api<Secret> = Api::namespaced(client.clone(), dst_ns);
    let pp = PatchParams::apply(crate::field_managers::CLAWSANDBOX).force();
    dst_api.patch(name, &pp, &Patch::Apply(patch)).await?;

    Ok(MirrorOutcome::Mirrored)
}

/// Append a ConfigMap volume + volumeMount + env-var into a partially-
/// built pod spec. Idempotent — calling twice with the same `volume_name`
/// is safe.
///
/// `pod_spec` is the JSON object under `template.spec` of a Deployment.
/// The function mutates it in place and returns the spec back for
/// chaining if desired.
pub fn inject_configmap_mount(
    pod_spec: &mut Value,
    container_name: &str,
    cm_name: &str,
    volume_name: &str,
    mount_path: &str,
    env_var: Option<(&str, &str)>,
) {
    inject_volume(
        pod_spec,
        volume_name,
        json!({ "configMap": { "name": cm_name, "optional": true } }),
    );
    inject_container_mount(pod_spec, container_name, volume_name, mount_path, env_var);
}

/// Append a Secret volume + volumeMount + env-var into a partially-built
/// pod spec.
pub fn inject_secret_mount(
    pod_spec: &mut Value,
    container_name: &str,
    secret_name: &str,
    volume_name: &str,
    mount_path: &str,
    env_var: Option<(&str, &str)>,
) {
    inject_volume(
        pod_spec,
        volume_name,
        json!({ "secret": { "secretName": secret_name, "optional": true } }),
    );
    inject_container_mount(pod_spec, container_name, volume_name, mount_path, env_var);
}

fn inject_volume(pod_spec: &mut Value, volume_name: &str, source: Value) {
    let obj = match pod_spec.as_object_mut() {
        Some(o) => o,
        None => return,
    };
    if !obj.get("volumes").map(Value::is_array).unwrap_or(false) {
        obj.insert("volumes".into(), json!([]));
    }
    let volumes = match obj.get_mut("volumes").and_then(|v| v.as_array_mut()) {
        Some(v) => v,
        None => return,
    };
    if volumes
        .iter()
        .any(|v| v.get("name").and_then(|n| n.as_str()) == Some(volume_name))
    {
        return;
    }
    let mut entry = json!({ "name": volume_name });
    let entry_obj = entry.as_object_mut().unwrap();
    if let Some(s) = source.as_object() {
        for (k, v) in s {
            entry_obj.insert(k.clone(), v.clone());
        }
    }
    volumes.push(entry);
}

/// Append only a container `volumeMount` (+ optional env var) to an
/// already-existing volume. Used when a single volume needs to be
/// mounted into multiple containers in the same pod (e.g. the compiled
/// AGT policy goes to both `inference-router` and `openclaw`).
pub fn inject_container_mount_public(
    pod_spec: &mut Value,
    container_name: &str,
    volume_name: &str,
    mount_path: &str,
    env_var: Option<(&str, &str)>,
) {
    inject_container_mount(pod_spec, container_name, volume_name, mount_path, env_var);
}

fn inject_container_mount(
    pod_spec: &mut Value,
    container_name: &str,
    volume_name: &str,
    mount_path: &str,
    env_var: Option<(&str, &str)>,
) {
    let containers = match pod_spec
        .get_mut("containers")
        .and_then(|c| c.as_array_mut())
    {
        Some(c) => c,
        None => return,
    };
    for container in containers.iter_mut() {
        if container.get("name").and_then(|n| n.as_str()) != Some(container_name) {
            continue;
        }
        let mounts = container
            .as_object_mut()
            .unwrap()
            .entry("volumeMounts")
            .or_insert(json!([]));
        if let Some(arr) = mounts.as_array_mut()
            && !arr
                .iter()
                .any(|m| m.get("name").and_then(|n| n.as_str()) == Some(volume_name))
        {
            arr.push(json!({
                "name": volume_name,
                "mountPath": mount_path,
                "readOnly": true,
            }));
        }
        if let Some((k, v)) = env_var {
            let env = container
                .as_object_mut()
                .unwrap()
                .entry("env")
                .or_insert(json!([]));
            if let Some(env_arr) = env.as_array_mut()
                && !env_arr
                    .iter()
                    .any(|e| e.get("name").and_then(|n| n.as_str()) == Some(k))
            {
                env_arr.push(json!({ "name": k, "value": v }));
            }
        }
    }
}

/// Set a single environment variable on a named container, idempotently.
///
/// Used when an env var must be set independently of a volume mount —
/// e.g. `MCP_JWKS_DIR` is set once for the parent directory after
/// per-server JWKS volumes have already been injected.
///
/// If the env var name already exists on the container, this is a no-op
/// (existing value wins — caller is responsible for ordering if updates
/// matter).
pub fn inject_container_env(pod_spec: &mut Value, container_name: &str, name: &str, value: &str) {
    let containers = match pod_spec
        .get_mut("containers")
        .and_then(|c| c.as_array_mut())
    {
        Some(c) => c,
        None => return,
    };
    for container in containers.iter_mut() {
        if container.get("name").and_then(|n| n.as_str()) != Some(container_name) {
            continue;
        }
        let env = container
            .as_object_mut()
            .unwrap()
            .entry("env")
            .or_insert(json!([]));
        if let Some(env_arr) = env.as_array_mut()
            && !env_arr
                .iter()
                .any(|e| e.get("name").and_then(|n| n.as_str()) == Some(name))
        {
            env_arr.push(json!({ "name": name, "value": value }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_router_pod_spec() -> Value {
        json!({
            "containers": [
                {
                    "name": "inference-router",
                    "image": "router:latest",
                    "env": [{"name": "EXISTING", "value": "1"}],
                },
                {
                    "name": "openclaw",
                    "image": "openclaw:latest",
                }
            ]
        })
    }

    #[test]
    fn injects_volume_and_mount_into_router_only() {
        let mut spec = empty_router_pod_spec();
        inject_configmap_mount(
            &mut spec,
            "inference-router",
            "toolpolicy-myref-profile",
            "tp-profile",
            "/etc/agt/policies",
            Some(("AGT_POLICY_DIR", "/etc/agt/policies")),
        );

        let volumes = spec.get("volumes").unwrap().as_array().unwrap();
        assert_eq!(volumes.len(), 1);
        assert_eq!(
            volumes[0].get("name").and_then(|v| v.as_str()),
            Some("tp-profile")
        );
        assert_eq!(
            volumes[0]
                .pointer("/configMap/name")
                .and_then(|v| v.as_str()),
            Some("toolpolicy-myref-profile")
        );

        let containers = spec.get("containers").unwrap().as_array().unwrap();
        let router = &containers[0];
        let mounts = router.get("volumeMounts").unwrap().as_array().unwrap();
        assert_eq!(mounts.len(), 1);
        assert_eq!(
            mounts[0].get("mountPath").and_then(|v| v.as_str()),
            Some("/etc/agt/policies")
        );

        // env var added
        let env = router.get("env").unwrap().as_array().unwrap();
        assert!(
            env.iter().any(
                |e| e.get("name").and_then(|n| n.as_str()) == Some("AGT_POLICY_DIR")
                    && e.get("value").and_then(|v| v.as_str()) == Some("/etc/agt/policies")
            ),
            "AGT_POLICY_DIR env var must be appended"
        );
        // pre-existing env preserved
        assert!(
            env.iter()
                .any(|e| e.get("name").and_then(|n| n.as_str()) == Some("EXISTING")),
            "pre-existing env preserved"
        );

        // openclaw container untouched
        let openclaw = &containers[1];
        assert!(
            openclaw.get("volumeMounts").is_none(),
            "non-router container must not get the mount"
        );
    }

    #[test]
    fn idempotent_double_inject_no_duplicates() {
        let mut spec = empty_router_pod_spec();
        for _ in 0..3 {
            inject_configmap_mount(
                &mut spec,
                "inference-router",
                "x-profile",
                "x-vol",
                "/etc/x",
                Some(("X_PATH", "/etc/x")),
            );
        }
        let volumes = spec.get("volumes").unwrap().as_array().unwrap();
        assert_eq!(volumes.len(), 1, "idempotent volumes");

        let mounts = spec
            .pointer("/containers/0/volumeMounts")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(mounts.len(), 1, "idempotent mounts");

        let env = spec
            .pointer("/containers/0/env")
            .unwrap()
            .as_array()
            .unwrap();
        let count = env
            .iter()
            .filter(|e| e.get("name").and_then(|n| n.as_str()) == Some("X_PATH"))
            .count();
        assert_eq!(count, 1, "idempotent env");
    }

    #[test]
    fn secret_mount_uses_secret_volume_source() {
        let mut spec = empty_router_pod_spec();
        inject_secret_mount(
            &mut spec,
            "inference-router",
            "mcp-foo-signing",
            "mcp-signing",
            "/etc/azureclaw/mcp-signing",
            None,
        );
        let v = spec.get("volumes").unwrap().as_array().unwrap();
        assert!(v[0].pointer("/secret/secretName").is_some());
        assert_eq!(
            v[0].pointer("/secret/secretName").and_then(|x| x.as_str()),
            Some("mcp-foo-signing")
        );
    }

    #[test]
    fn omits_env_when_none() {
        let mut spec = empty_router_pod_spec();
        let env_before = spec.pointer("/containers/0/env").cloned();
        inject_configmap_mount(
            &mut spec,
            "inference-router",
            "card",
            "card-vol",
            "/etc/azureclaw/a2a-card",
            None,
        );
        let env_after = spec.pointer("/containers/0/env");
        assert_eq!(env_before.as_ref(), env_after);
    }

    #[test]
    fn missing_target_container_is_noop_for_mounts() {
        let mut spec = json!({
            "containers": [{"name": "other", "image": "x"}]
        });
        inject_configmap_mount(
            &mut spec,
            "inference-router",
            "cm",
            "vol",
            "/etc",
            Some(("X", "/etc")),
        );
        // volume still appended (not container-specific)
        assert!(spec.get("volumes").unwrap().as_array().unwrap().len() == 1);
        // but no mount on `other`
        assert!(
            spec.pointer("/containers/0/volumeMounts").is_none(),
            "non-target container untouched"
        );
    }

    #[test]
    fn inject_container_env_adds_var_to_named_container_only() {
        let mut spec = empty_router_pod_spec();
        inject_container_env(
            &mut spec,
            "inference-router",
            "MCP_JWKS_DIR",
            "/etc/azureclaw/mcp",
        );

        let env = spec
            .pointer("/containers/0/env")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(env.len(), 2);
        assert!(env.iter().any(
            |e| e.get("name").and_then(|n| n.as_str()) == Some("MCP_JWKS_DIR")
                && e.get("value").and_then(|v| v.as_str()) == Some("/etc/azureclaw/mcp")
        ));
        // Other container untouched.
        assert!(spec.pointer("/containers/1/env").is_none());
    }

    #[test]
    fn inject_container_env_is_idempotent_when_var_already_set() {
        let mut spec = empty_router_pod_spec();
        inject_container_env(&mut spec, "inference-router", "EXISTING", "different-value");
        let env = spec
            .pointer("/containers/0/env")
            .unwrap()
            .as_array()
            .unwrap();
        // No duplicate inserted; original value preserved.
        assert_eq!(env.len(), 1);
        assert_eq!(
            env[0].get("value").and_then(|v| v.as_str()),
            Some("1"),
            "existing value wins; caller is responsible for ordering",
        );
    }

    #[test]
    fn inject_container_env_creates_env_array_when_missing() {
        let mut spec = empty_router_pod_spec();
        // The openclaw container has no "env" key.
        inject_container_env(&mut spec, "openclaw", "FOO", "bar");
        let env = spec
            .pointer("/containers/1/env")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].get("name").and_then(|n| n.as_str()), Some("FOO"));
    }

    #[test]
    fn inject_container_env_no_op_when_container_absent() {
        let mut spec = empty_router_pod_spec();
        let before = spec.clone();
        inject_container_env(&mut spec, "does-not-exist", "X", "y");
        assert_eq!(spec, before);
    }

    #[test]
    fn safe_label_value_passes_through_short_values() {
        assert_eq!(
            super::safe_label_value("inferencepolicy.my-policy"),
            "inferencepolicy.my-policy"
        );
        let exact = "a".repeat(63);
        assert_eq!(super::safe_label_value(&exact), exact);
    }

    #[test]
    fn safe_label_value_truncates_with_stable_hash_when_too_long() {
        // Real-world regression: telegram-agent's InferencePolicy
        // mirror produced a 64-byte label value before this guard.
        let long = "inferencepolicy.inferencepolicy-telegram-agent-inference-profile";
        assert_eq!(long.len(), 64);
        let safe = super::safe_label_value(long);
        assert!(safe.len() <= 63, "got {} bytes: {safe}", safe.len());
        // Stable: same input → same output.
        assert_eq!(super::safe_label_value(long), safe);
        // Different inputs → different outputs (collision-resistant).
        let other = "inferencepolicy.inferencepolicy-different-name-foo-profile";
        assert_ne!(super::safe_label_value(other), safe);
    }
}
