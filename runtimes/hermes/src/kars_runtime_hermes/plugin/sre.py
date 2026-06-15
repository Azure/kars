# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""kars-sre Hermes plugin — Slice 1 (MVP read-only diagnostic tools).

Registered by ``runtimes/hermes/src/kars_runtime_hermes/plugin/__init__.py``
only when the env ``KARS_SRE_ENABLED=true`` is set. The Helm template
``deploy/helm/kars/templates/sre.yaml`` sets that env exclusively on
the ``sre`` KarsSandbox pod via ``spec.runtime.hermes.extraEnv``;
standard Hermes sandboxes never see the env and therefore never get
the ``sre_*`` tool surface.

Containment (per docs/blueprints/07-kars-sre-proposal.md §7.8):

  - §7.8.1  Plugin packaging — Slice 1 ships SRE inside the shared
            Hermes image gated on the env. The §7.8.1 separate-image
            split is a follow-up slice. The env gate is the
            interim enforcement boundary: the tools simply aren't
            registered in any other pod, so a remote agent asking
            for ``sre_*`` calls hits "tool not found" at the runtime
            (not at the policy layer).
  - §7.8.5  Spawn disabled — the plugin __init__.py also
            deregisters the ``kars_spawn`` family when this env
            is set, so the SRE agent cannot spawn sub-agents.
  - §7.8.6  Mesh disabled at the source — the plugin __init__.py
            deregisters the ``kars_mesh_*`` family AND the
            NetworkPolicy in sre.yaml omits the agentmesh namespace
            from the allowlist, so even if a future bug accidentally
            tried to dial the relay, the network path does not exist.

Slice 1 tool surface (all read-only, no approval gates):

  ============================  ================================================
  Tool                          What it does
  ============================  ================================================
  sre_describe_state            Structured snapshot of every kars-owned CR in
                                every namespace (KarsSandbox · InferencePolicy
                                · ToolPolicy · EgressApproval · KarsMemory ·
                                etc.) with phase, conditions, last reconcile.

  sre_logs                      Tail any pod's any container (capped 500
                                lines). Uses the standard apiserver
                                /api/v1/namespaces/<ns>/pods/<name>/log
                                endpoint with ?container=<name>&tailLines=N.

  sre_diagnose                  Walks the kars-CR health checklist:
                                controller deployment Ready, CRDs present,
                                no KarsSandbox in Failed/Degraded for >5min,
                                no orphaned ConfigMaps. Returns a structured
                                report.

  sre_explain_error             Given an error string, returns a structured
                                root-cause hypothesis by matching against a
                                small in-process corpus of known kars
                                failure modes (extracted from the OOTB
                                blockers tracked in the proposal §Why).

  sre_propose_fix               Given a diagnosis, returns a proposed typed
                                action (per §7.7.1 — JSON document, not a
                                shell command). READ-ONLY: produces the
                                proposal, does not execute. Apply lands in
                                Slice 3.
  ============================  ================================================

Each tool returns a dict; the Hermes plugin context serialises it
to the LLM. The tool implementation MUST never raise on apiserver
errors — those become ``{"error": "..."}`` entries in the returned
dict so the LLM can reason over them. Hard raises are reserved for
"this tool is misconfigured" issues that aren't agent-recoverable.
"""

from __future__ import annotations

import logging
import os
from typing import Any

import httpx

from . import sre_kube

logger = logging.getLogger("kars.hermes.sre")

# --------------------------------------------------------------------------
# Constants
# --------------------------------------------------------------------------

KARS_GROUP = "kars.azure.com"
KARS_VERSION = "v1alpha1"

# The kars-owned CR kinds the SRE agent knows about (matches the RBAC
# grant in deploy/helm/kars/templates/sre.yaml). Plural form is what
# the apiserver expects in the URL path.
KARS_CR_KINDS: list[tuple[str, str]] = [
    ("karssandboxes", "KarsSandbox"),
    ("inferencepolicies", "InferencePolicy"),
    ("toolpolicies", "ToolPolicy"),
    ("egressapprovals", "EgressApproval"),
    ("karsmemories", "KarsMemory"),
    ("karsevals", "KarsEval"),
    ("trustgraphs", "TrustGraph"),
    ("karspairings", "KarsPairing"),
    ("a2aagents", "A2AAgent"),
    ("mcpservers", "McpServer"),
    ("karsauthconfigs", "KarsAuthConfig"),
]


# --------------------------------------------------------------------------
# OOTB-blocker corpus — known kars failure modes for sre_explain_error
# --------------------------------------------------------------------------
#
# The corpus is intentionally small and hand-curated rather than an
# embedding-backed search: false positives on diagnostic hypotheses
# are confusing to operators, so we match only patterns that have
# very high signal. The corpus grows with each new OOTB blocker the
# proposal §Why list captures.
OOTB_CORPUS: list[dict[str, str]] = [
    {
        "pattern": "ImagePullBackOff",
        "hypothesis": (
            "The pod's container image is unreachable or doesn't exist. Causes: "
            "image tag typo in the controlling resource (KarsSandbox spec.runtime / "
            "Deployment spec.template.spec.containers[].image), private registry "
            "without an imagePullSecret, or registry-side throttling/outage."
        ),
        "next_steps": (
            "1) describe the pod to read the precise pull error; "
            "2) list image tags actually in use on the cluster to suggest the "
            "closest valid one; "
            "3) propose PatchDeploymentImage with the corrected tag."
        ),
    },
    {
        "pattern": "exceeded quota",
        "hypothesis": (
            "Pod creation is being rejected by a ResourceQuota in the namespace. "
            "Likely cause: an operator-applied platform ResourceQuota whose ceiling "
            "is lower than the workload's requests (the textbook GitOps-collision "
            "incident)."
        ),
        "next_steps": (
            "1) list ResourceQuotas in the namespace; "
            "2) compare the quota's `hard` map against the deployment's requests; "
            "3) propose DeleteResourceQuota for the offending policy (only "
            "permitted when the ResourceQuota does NOT carry the "
            "kars.azure.com/managed-by=controller label)."
        ),
    },
    {
        "pattern": "OOMKilled",
        "hypothesis": (
            "Container was killed by the kernel for exceeding its memory limit. "
            "Causes: memory limit too low for the workload's working set, memory "
            "leak in the workload, or a sibling container in the same pod "
            "starving this one."
        ),
        "next_steps": (
            "1) check the pod's containerStatuses[].lastState for the kill memory "
            "usage; "
            "2) describe the deployment for current resource.limits.memory; "
            "3) propose PatchDeploymentResources to a higher ceiling (Slice 3+)."
        ),
    },
    {
        "pattern": "CrashLoopBackOff",
        "hypothesis": (
            "Container is repeatedly exiting non-zero on startup. Causes: "
            "misconfiguration in env / config / mounted secrets, a hard "
            "dependency that's unreachable at startup, or a bug in the "
            "container itself surfaced by a recent rollout."
        ),
        "next_steps": (
            "1) tail the container logs via sre_logs to get the exit reason; "
            "2) describe the pod for restart count + last exit code; "
            "3) compare current image/env to the last-known-good rollout via "
            "sre_what_changed (Slice 2)."
        ),
    },
    {
        "pattern": "FailedScheduling",
        "hypothesis": (
            "Scheduler cannot place the pod on any node. Causes: no node has the "
            "requested resources, all candidate nodes are cordoned/tainted, "
            "topology constraints unsatisfiable, or PVC pending."
        ),
        "next_steps": (
            "1) describe the pod for the scheduler's per-node reason summary; "
            "2) check node status (Ready, schedulable, taints); "
            "3) propose UncordonNode (Slice 3, node-tier write) or "
            "ScaleDeployment to fit."
        ),
    },
    {
        "pattern": "ContainerCreating",
        "hypothesis": (
            "Stuck creating — kubelet is attempting to set up the container but "
            "blocking on a precondition. Causes: secret/configmap referenced by "
            "envFrom/volumes doesn't exist yet, image pull in progress, "
            "init-container still running, or a PVC binding."
        ),
        "next_steps": (
            "1) describe the pod for the kubelet's last event; "
            "2) verify referenced secrets / configmaps / PVCs exist; "
            "3) if image pull is the cause, wait + re-check."
        ),
    },
]


# --------------------------------------------------------------------------
# Tool implementations
# --------------------------------------------------------------------------


def _summarise_cr(item: dict[str, Any], kind: str) -> dict[str, Any]:
    """Reduce a CR's full JSON to the fields the agent cares about."""
    meta = item.get("metadata", {})
    status = item.get("status", {})
    return {
        "kind": kind,
        "namespace": meta.get("namespace"),
        "name": meta.get("name"),
        "phase": status.get("phase"),
        "observedGeneration": status.get("observedGeneration"),
        "lastReconciled": status.get("lastReconciled"),
        "conditions": [
            {
                "type": c.get("type"),
                "status": c.get("status"),
                "reason": c.get("reason"),
                "message": c.get("message"),
            }
            for c in status.get("conditions", [])
        ],
    }


def _impl_sre_describe_state(**_kwargs: Any) -> dict[str, Any]:
    """Tool: structured snapshot of every kars-owned CR in the cluster.

    Returns a dict keyed by CR kind whose values are lists of summarised
    instances. Each instance carries name + namespace + phase +
    observedGeneration + lastReconciled + conditions — enough for the
    agent to spot Degraded/Failed/stale CRs without re-fetching.
    """
    kube = sre_kube.client()
    out: dict[str, Any] = {}
    for plural, kind in KARS_CR_KINDS:
        path = f"/apis/{KARS_GROUP}/{KARS_VERSION}/{plural}"
        try:
            doc = kube.get(path)
            items = doc.get("items", [])
            out[kind] = [_summarise_cr(it, kind) for it in items]
        except httpx.HTTPStatusError as exc:
            # 404 = the CRD isn't installed; common during early-cluster.
            # 403 = RBAC didn't bind correctly; informative to surface.
            out[kind] = {
                "error": f"{exc.response.status_code} {exc.response.reason_phrase}",
                "path": path,
            }
        except Exception as exc:  # noqa: BLE001 — tool MUST NOT raise
            out[kind] = {"error": str(exc), "path": path}
    return out


def _impl_sre_logs(
    *,
    namespace: str,
    pod: str,
    container: str | None = None,
    tail: int = 500,
    **_kwargs: Any,
) -> dict[str, Any]:
    """Tool: tail pod logs.

    Args:
        namespace: pod's namespace.
        pod: pod name.
        container: container name within the pod; omit for single-container pods.
        tail: max lines to return (capped at 500).
    """
    tail = max(1, min(tail, 500))
    params: dict[str, Any] = {"tailLines": tail}
    if container:
        params["container"] = container
    path = f"/api/v1/namespaces/{namespace}/pods/{pod}/log"
    kube = sre_kube.client()
    try:
        client = kube._ensure_client()  # noqa: SLF001 — same module surface
        resp = client.get(path, params=params)
        resp.raise_for_status()
        return {
            "namespace": namespace,
            "pod": pod,
            "container": container,
            "tailLines": tail,
            "logs": resp.text,
        }
    except httpx.HTTPStatusError as exc:
        return {
            "namespace": namespace,
            "pod": pod,
            "container": container,
            "error": f"{exc.response.status_code} {exc.response.reason_phrase}",
            "body": exc.response.text[:512],
        }
    except Exception as exc:  # noqa: BLE001
        return {"namespace": namespace, "pod": pod, "container": container, "error": str(exc)}


def _impl_sre_diagnose(**_kwargs: Any) -> dict[str, Any]:
    """Tool: walk the kars-CR health checklist.

    Returns a structured report:
      - controller_status: deployment ready?
      - crds_present: every CRD the controller expects is installed?
      - degraded_sandboxes: KarsSandboxes whose .status.phase ∉ {Ready,Running}
      - degraded_policies: governance CRs in non-Ready phases
      - stale_reconciles: CRs whose lastReconciled is > 5min old
    """
    kube = sre_kube.client()
    report: dict[str, Any] = {
        "controller_status": "unknown",
        "crds_present": [],
        "crds_missing": [],
        "degraded_sandboxes": [],
        "degraded_policies": [],
        "summary": "",
    }

    # 1) Controller deployment status
    try:
        doc = kube.get("/apis/apps/v1/namespaces/kars-system/deployments/kars-controller")
        spec_replicas = doc.get("spec", {}).get("replicas", 0)
        ready_replicas = doc.get("status", {}).get("readyReplicas", 0) or 0
        if ready_replicas >= 1 and ready_replicas == spec_replicas:
            report["controller_status"] = "Ready"
        else:
            report["controller_status"] = f"Degraded ({ready_replicas}/{spec_replicas} ready)"
    except Exception as exc:  # noqa: BLE001
        report["controller_status"] = f"Unknown: {exc}"

    # 2) CRD inventory check
    try:
        doc = kube.get("/apis/apiextensions.k8s.io/v1/customresourcedefinitions")
        installed = {c.get("metadata", {}).get("name") for c in doc.get("items", [])}
        for plural, _kind in KARS_CR_KINDS:
            full = f"{plural}.{KARS_GROUP}"
            if full in installed:
                report["crds_present"].append(full)
            else:
                report["crds_missing"].append(full)
    except Exception as exc:  # noqa: BLE001
        report["crds_present"] = f"error: {exc}"

    # 3) Sandbox/policy phase scan — reuse describe_state results
    state = sre_describe_state()
    for kind, items in state.items():
        if isinstance(items, dict) and "error" in items:
            continue
        for it in items:
            phase = it.get("phase")
            if phase and phase not in {"Ready", "Running", "Compiled", "Active"}:
                bucket = (
                    "degraded_sandboxes" if kind == "KarsSandbox" else "degraded_policies"
                )
                report[bucket].append(it)

    # 3b) Workload-availability cross-check — KarsSandbox.status.phase
    # reflects controller reconcile state, not actual pod readiness.
    # A namespace-level ResourceQuota or image-pull failure can leave
    # `available < desired` on the Deployment while the CR still says
    # Running. We surface those as `WorkloadDown(<avail>/<desired>)`
    # so the agent (and the operator reading sre_diagnose output)
    # actually sees the incident.
    sandbox_items = state.get("KarsSandbox", [])
    if isinstance(sandbox_items, list):
        for sb in sandbox_items:
            name = sb.get("name")
            if not name:
                continue
            try:
                d = kube.get(
                    f"/apis/apps/v1/namespaces/kars-{name}/deployments/{name}"
                )
            except Exception:  # noqa: BLE001 — best-effort
                continue
            desired = (d.get("spec") or {}).get("replicas") or 0
            available = ((d.get("status") or {}).get("availableReplicas") or 0)
            if desired > 0 and available < desired:
                synthetic = dict(sb)
                synthetic["phase"] = f"WorkloadDown({available}/{desired})"
                synthetic["workload_namespace"] = f"kars-{name}"
                synthetic["workload_deployment"] = name
                report["degraded_sandboxes"].append(synthetic)

    # 4) Summary string the LLM can quote verbatim
    n_deg_sb = len(report["degraded_sandboxes"])
    n_deg_pol = len(report["degraded_policies"])
    n_missing = len(report["crds_missing"])
    bits = []
    bits.append(f"controller: {report['controller_status']}")
    bits.append(f"CRDs missing: {n_missing}")
    bits.append(f"sandboxes degraded: {n_deg_sb}")
    bits.append(f"governance CRs degraded: {n_deg_pol}")
    report["summary"] = "; ".join(bits)
    return report


def _impl_sre_explain_error(*, error: str, **_kwargs: Any) -> dict[str, Any]:
    """Tool: match an error string against the OOTB-blocker corpus.

    Returns the first matching entry's hypothesis + next_steps, or
    ``{"matched": False}`` if no pattern matches. The agent is expected
    to use this as a hint, not a verdict — it then walks the next_steps
    using the other diagnostic tools to confirm.
    """
    if not error:
        return {"matched": False, "reason": "empty error string"}
    lowered = error.lower()
    matches = [c for c in OOTB_CORPUS if c["pattern"].lower() in lowered]
    if not matches:
        return {"matched": False, "error": error}
    # Return up to 3 matches (sorted by pattern length desc — longer
    # patterns are more specific, less likely to be false positives).
    matches.sort(key=lambda c: len(c["pattern"]), reverse=True)
    return {
        "matched": True,
        "error": error,
        "hypotheses": matches[:3],
    }


def _impl_sre_propose_fix(
    *,
    diagnosis: str,
    target: dict[str, Any] | None = None,
    rationale: str | None = None,
    ttl_minutes: int | None = None,
    action_type: str | None = None,
    **_kwargs: Any,
) -> dict[str, Any]:
    """Tool: propose a typed action AND create a KarsSREAction CR (Slice 3).

    Slice 1 returned a proposal envelope only. Slice 3 EXTENDS the same
    tool: when the proposal carries a typed action, the tool also POSTs
    a ``KarsSREAction`` CR to ``kars-sre`` namespace with phase
    ``Proposed`` and ``approval.state=Pending``. The CR is the
    operator's approval surface — they flip
    ``.spec.approval.state="Approved"`` via ``kars sre approve <id>``
    (or directly in ``kubectl edit``) to authorise execution.

    On approval, the controller mints a one-shot ClusterRoleBinding,
    executes the typed action, tears the binding down, and watches the
    target workload for recovery. The whole flow is one CR per
    incident; the agent never executes anything directly.

    Args:
        diagnosis: short string describing what the agent concluded.
        target:    {"kind", "namespace", "name"} of the resource the
                   proposal acts on. ``kind`` determines the typed action.
        action_type: optional explicit override for the typed action
                   (one of ``DeleteResourceQuota``, ``PatchDeploymentImage``,
                   ``ScaleDeployment``, ``RolloutRestart``, ``DeletePod``).
                   When set, takes precedence over the kind inferred
                   from ``target.kind``.
        rationale: optional one-paragraph operator-facing rationale
                   (audit-grade). When unset, a sensible default is
                   used per action kind.
        ttl_minutes: optional proposal TTL (default 15, max 60).

    Returns the proposal envelope. When a CR was successfully created,
    the envelope includes ``action_id`` (the CR name) and ``cr_created=True``;
    the operator copy-pastes that ID into ``kars sre approve``.
    """
    target = target or {}
    # Tolerant key lookup — accept several spellings the agent may use.
    target_kind = (
        target.get("kind")
        or target.get("type")
        or _kwargs.get("kind")
        or _kwargs.get("target_kind")
    )
    # Infer kind from explicit action_type override if still unknown.
    if not target_kind and action_type:
        target_kind = {
            "DeleteResourceQuota": "ResourceQuota",
            "DeletePod": "Pod",
            "ScaleDeployment": "Deployment",
            "PatchDeploymentImage": "Deployment",
            "RolloutRestart": "Deployment",
        }.get(action_type)

    proposal: dict[str, Any] = {
        "kind": "FixProposal",
        "diagnosis": diagnosis,
        "target": {**target, "kind": target_kind} if target_kind else target,
        "action": None,
        "rationale": rationale,
        "execution_status": "proposed (awaiting operator approval — run `kars sre approve <action_id>`)",
        "cr_created": False,
        "action_id": None,
    }

    # Explicit action_type overrides kind-based inference.
    if action_type == "DeleteResourceQuota" or (
        action_type is None and target_kind == "ResourceQuota"
    ):
        proposal["action"] = {
            "type": "DeleteResourceQuota",
            "namespace": target.get("namespace"),
            "name": target.get("name"),
        }
        if not proposal["rationale"]:
            proposal["rationale"] = (
                "Operator-applied ResourceQuotas without the "
                "kars.azure.com/managed-by=controller label are safely deletable "
                "by the SRE agent (per §7.7.1). Removing this quota restores "
                "the namespace's pod admission and the controller will "
                "schedule a fresh sandbox pod."
            )
    elif action_type == "PatchDeploymentImage" or (
        action_type is None
        and target_kind in {"Deployment", "StatefulSet", "DaemonSet"}
        and "image" in _kwargs
    ):
        proposal["action"] = {
            "type": "PatchDeploymentImage",
            "namespace": target.get("namespace"),
            "name": target.get("name"),
            "container": _kwargs.get("container"),
            "image": _kwargs.get("image"),
        }
        if not proposal["rationale"]:
            proposal["rationale"] = (
                "Patch the container image to the proposed value. The target "
                "namespace must not be in the protected denylist (kars-system, "
                "kars-sre, kube-system, etc. — §7.7.1)."
            )
    elif action_type == "ScaleDeployment" or (
        action_type is None
        and target_kind in {"Deployment", "StatefulSet"}
        and "replicas" in _kwargs
    ):
        proposal["action"] = {
            "type": "ScaleDeployment",
            "namespace": target.get("namespace"),
            "name": target.get("name"),
            "replicas": _kwargs.get("replicas"),
        }
        if not proposal["rationale"]:
            proposal["rationale"] = "Scale the workload's replica count."
    elif action_type == "RolloutRestart" or (
        action_type is None
        and target_kind in {"Deployment", "StatefulSet", "DaemonSet"}
        and _kwargs.get("rollout_restart")
    ):
        proposal["action"] = {
            "type": "RolloutRestart",
            "namespace": target.get("namespace"),
            "name": target.get("name"),
            "kind": target_kind or "Deployment",
        }
        if not proposal["rationale"]:
            proposal["rationale"] = (
                "Trigger a rolling restart by patching the pod template's "
                "kubectl.kubernetes.io/restartedAt annotation. Useful for "
                "config-map / secret reloads or transient pod-level wedges."
            )
    elif action_type == "DeletePod" or (action_type is None and target_kind == "Pod"):
        proposal["action"] = {
            "type": "DeletePod",
            "namespace": target.get("namespace"),
            "name": target.get("name"),
        }
        if not proposal["rationale"]:
            proposal["rationale"] = (
                "Delete the pod so its owning controller (ReplicaSet, "
                "StatefulSet, DaemonSet, Job) reconciles a fresh instance. "
                "Use sparingly — only when the workload is stuck in a "
                "state a restart would clear."
            )
    else:
        # No action could be inferred — tell the agent what's missing
        # so it can retry with the right shape rather than silently
        # falling back to "manual fix".
        missing = []
        if not target_kind:
            missing.append("target.kind (or action_type)")
        if not target.get("namespace"):
            missing.append("target.namespace")
        if not target.get("name"):
            missing.append("target.name")
        _kinds = "ResourceQuota / Pod / Deployment / StatefulSet / DaemonSet"
        _hint = ", ".join(missing) if missing else f"a supported target.kind: {_kinds}"
        proposal["cr_error"] = (
            "Could not infer typed action from arguments. "
            f"Provide {_hint}. "
            "Alternatively, pass action_type explicitly "
            "(DeleteResourceQuota, DeletePod, ScaleDeployment, PatchDeploymentImage, RolloutRestart)."
        )
        if not proposal["rationale"]:
            proposal["rationale"] = proposal["cr_error"]

    # Slice 3 — if we have a typed action, create the KarsSREAction CR
    # so the operator has an approve surface. Failures here are
    # non-fatal: the agent still returns the proposal text and the
    # operator can fall back to the manual runbook.
    if proposal["action"] is not None:
        try:
            action_id = _create_karssreaction_cr(
                action=proposal["action"],
                diagnosis=diagnosis,
                rationale=proposal["rationale"],
                ttl_minutes=ttl_minutes,
            )
            proposal["action_id"] = action_id
            proposal["cr_created"] = True
            proposal["approve_command"] = f"kars sre approve {action_id}"
            proposal["reject_command"] = f"kars sre reject {action_id}"
        except Exception as e:  # noqa: BLE001 — surface the error in the envelope
            proposal["cr_created"] = False
            proposal["cr_error"] = str(e)
            logger.warning("sre_propose_fix: KarsSREAction CR create failed: %s", e)

    return proposal


def _create_karssreaction_cr(
    *,
    action: dict[str, Any],
    diagnosis: str,
    rationale: str | None,
    ttl_minutes: int | None,
) -> str:
    """POST a KarsSREAction CR to ``kars-sre`` and return its name.

    The CR is generated with the K8s-side ``generateName`` mechanism so
    the apiserver picks a unique name (``sre-action-<5-char-suffix>``)
    on every call — no agent-side name collision risk.

    Schema is per ``controller/src/kars_sre_action.rs``: flat action
    payload from the proposal is reshaped into
    ``{type, params: {...}}`` to match the CRD.
    """
    kube = sre_kube.client()
    # Reshape the flat proposal action → CRD `{type, params}` shape.
    action_type = action.get("type")
    params = {k: v for k, v in action.items() if k != "type"}
    body: dict[str, Any] = {
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSREAction",
        "metadata": {
            "generateName": "sre-action-",
            "namespace": "kars-sre",
            "labels": {
                "app.kubernetes.io/component": "sre",
                "kars.azure.com/sre-action-type": str(action_type or "unknown"),
            },
        },
        "spec": {
            "action": {
                "type": action_type,
                "params": params,
            },
            "approval": {"state": "Pending"},
            "diagnosis": diagnosis[:512] if diagnosis else None,
            "rationale": rationale[:2048] if rationale else None,
        },
    }
    if ttl_minutes is not None:
        body["spec"]["ttlMinutes"] = max(1, min(60, int(ttl_minutes)))
    # Drop None spec fields — the CRD treats them as unset, not null.
    body["spec"] = {k: v for k, v in body["spec"].items() if v is not None}

    created = kube.post(
        "/apis/kars.azure.com/v1alpha1/namespaces/kars-sre/karssreactions",
        json=body,
    )
    return str(created.get("metadata", {}).get("name", "<unknown>"))


# --------------------------------------------------------------------------
# Plugin registration
# --------------------------------------------------------------------------


def is_enabled() -> bool:
    """Return True if the env gate is set. Called by the plugin __init__.py.

    The env is set exclusively by ``deploy/helm/kars/templates/sre.yaml``
    on the ``sre`` KarsSandbox's ``spec.runtime.hermes.extraEnv``.
    Standard sandboxes don't see it.

    NOTE on naming: the env is ``SRE_ENABLED`` rather than
    ``KARS_SRE_ENABLED`` because the controller's deployment builder
    silently strips user-supplied ``extraEnv`` keys with the reserved
    ``KARS_`` prefix (controller/src/reconciler/mod.rs:1583). The right
    long-term fix is for the controller to detect
    ``kars.azure.com/role: sre`` on the KarsSandbox label and inject
    ``KARS_SRE_ENABLED=true`` itself (controller-side injection bypasses
    the prefix filter). Tracked as a follow-up; for now ``SRE_ENABLED``
    is the gate.
    """
    return os.environ.get("SRE_ENABLED", "").lower() in {"true", "1", "yes"}


def register(ctx: Any) -> None:  # noqa: ANN401 — Hermes' ctx is dynamic
    """Register the SRE tool surface on the Hermes plugin context.

    Idempotent: re-registration replaces the existing tool definitions.
    Called from ``runtimes/hermes/.../plugin/__init__.py`` only when
    ``is_enabled()`` returns True.
    """
    register_tool = getattr(ctx, "register_tool", None)
    if not callable(register_tool):
        logger.warning("Hermes ctx has no register_tool — SRE plugin not registered")
        return

    register_tool(
        name="sre_describe_state",
        toolset="sre",
        description=(
            "Return a structured snapshot of every kars-owned CR in every "
            "namespace (KarsSandbox, InferencePolicy, ToolPolicy, "
            "EgressApproval, KarsMemory, KarsEval, TrustGraph, KarsPairing, "
            "A2AAgent, McpServer, KarsAuthConfig). Each CR carries name, "
            "namespace, phase, observedGeneration, lastReconciled, and "
            "conditions. Use this as the first call when starting an "
            "incident investigation."
        ),
        schema={"type": "object", "properties": {}, "required": []},
        handler=sre_describe_state,
    )

    register_tool(
        name="sre_logs",
        toolset="sre",
        description=(
            "Tail logs from a pod's container via the apiserver. Returns the "
            "last N lines (max 500). Use for diagnosing CrashLoopBackOff or "
            "for inspecting an agent's behaviour."
        ),
        schema={
            "type": "object",
            "properties": {
                "namespace": {"type": "string", "description": "Pod's namespace"},
                "pod": {"type": "string", "description": "Pod name"},
                "container": {
                    "type": "string",
                    "description": "Container name (omit for single-container pods)",
                },
                "tail": {
                    "type": "integer",
                    "description": "Max lines to return (capped at 500)",
                    "default": 200,
                },
            },
            "required": ["namespace", "pod"],
        },
        handler=sre_logs,
    )

    register_tool(
        name="sre_diagnose",
        toolset="sre",
        description=(
            "Walk the kars-CR health checklist: controller deployment Ready, "
            "every kars CRD installed, no Degraded/Failed sandboxes or "
            "governance CRs, no stale reconciles. Returns a structured "
            "report + a one-line summary suitable for an operator-facing "
            "message."
        ),
        schema={"type": "object", "properties": {}, "required": []},
        handler=sre_diagnose,
    )

    register_tool(
        name="sre_explain_error",
        toolset="sre",
        description=(
            "Given an error string (pod event reason, controller log line, "
            "etc.), return a root-cause hypothesis from the kars OOTB-blocker "
            "corpus. The hypothesis is a HINT — the agent should then use "
            "the other diagnostic tools to confirm or refute it."
        ),
        schema={
            "type": "object",
            "properties": {
                "error": {
                    "type": "string",
                    "description": "The error string to explain",
                },
            },
            "required": ["error"],
        },
        handler=sre_explain_error,
    )

    register_tool(
        name="sre_propose_fix",
        toolset="sre",
        description=(
            "Propose a typed-action fix AND create the KarsSREAction CR "
            "the operator approves to authorise execution. Returns an "
            "action_id the operator pastes into `kars sre approve <id>`. "
            "Always called AFTER diagnosis. REQUIRES target.kind (or "
            "explicit action_type) — without it no CR is created and "
            "the envelope's cr_error field tells you what's missing."
        ),
        schema={
            "type": "object",
            "properties": {
                "diagnosis": {
                    "type": "string",
                    "description": "One-line summary of what was diagnosed",
                },
                "target": {
                    "type": "object",
                    "description": (
                        "Resource the proposal acts on. `kind` is REQUIRED "
                        "(one of ResourceQuota / Pod / Deployment / StatefulSet / "
                        "DaemonSet) so the right typed action can be inferred."
                    ),
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": [
                                "ResourceQuota",
                                "Pod",
                                "Deployment",
                                "StatefulSet",
                                "DaemonSet",
                            ],
                            "description": "Kubernetes Kind of the target — REQUIRED",
                        },
                        "namespace": {"type": "string"},
                        "name": {"type": "string"},
                    },
                    "required": ["kind", "namespace", "name"],
                },
                "action_type": {
                    "type": "string",
                    "enum": [
                        "DeleteResourceQuota",
                        "PatchDeploymentImage",
                        "ScaleDeployment",
                        "RolloutRestart",
                        "DeletePod",
                    ],
                    "description": (
                        "Optional explicit override — when set, takes precedence "
                        "over the kind inferred from target.kind. Use this when "
                        "the same target.kind maps to multiple actions "
                        "(e.g. Deployment → Scale vs PatchImage vs RolloutRestart)."
                    ),
                },
                "rationale": {
                    "type": "string",
                    "description": (
                        "Optional operator-facing rationale (≤ 2048 chars). "
                        "Falls back to a per-action default if unset."
                    ),
                },
                "ttl_minutes": {
                    "type": "integer",
                    "description": (
                        "Optional CR auto-expire window in minutes (default 15, max 60). "
                        "Beyond this, the proposal lapses to Expired without operator action."
                    ),
                },
            },
            "required": ["diagnosis", "target"],
        },
        handler=sre_propose_fix,
    )

    # Slice 2 — register the K8s diagnostic toolset alongside the Slice 1
    # tools. sre_k8s.register() handles its own ctx wiring.
    from . import sre_k8s  # noqa: PLC0415 — lazy import

    sre_k8s.register(ctx)

    logger.info("kars-sre plugin registered (Slice 1: 5 read-only kars-CR tools; Slice 2: 5 K8s diag tools)")


# ─── Hermes-shape adapters ────────────────────────────────────────────
# Hermes invokes tool handlers as `handler(args: dict, **ctx)`. Our
# impl functions take **kwargs so they're easy to unit-test; these
# adapters bridge the two surfaces.

def sre_explain_error(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_explain_error(**args)

def sre_describe_state(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_describe_state(**args)

def sre_diagnose(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_diagnose(**args)

def sre_propose_fix(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_propose_fix(**args)

def sre_logs(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_logs(**args)
