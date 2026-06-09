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


def sre_describe_state(**_kwargs: Any) -> dict[str, Any]:
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


def sre_logs(
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


def sre_diagnose(**_kwargs: Any) -> dict[str, Any]:
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


def sre_explain_error(*, error: str, **_kwargs: Any) -> dict[str, Any]:
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


def sre_propose_fix(
    *,
    diagnosis: str,
    target: dict[str, Any] | None = None,
    **_kwargs: Any,
) -> dict[str, Any]:
    """Tool: propose a typed action (read-only — no execution).

    Args:
        diagnosis: short string describing what the agent has concluded
                   (e.g. "ResourceQuota platform-hardening-quota in
                   kars-research is blocking pod admission").
        target:    optional dict carrying the resource the proposal acts on,
                   e.g. {"kind": "ResourceQuota", "namespace": "kars-research",
                         "name": "platform-hardening-quota"}.

    Returns a proposal envelope with the typed-action payload. Slice 1
    is read-only: the proposal is returned to the agent (who relays it
    to the operator); Slice 3 (`sre_apply_fix`) adds the execution
    path with TokenRequest + admission gate.
    """
    target = target or {}
    proposal: dict[str, Any] = {
        "kind": "FixProposal",
        "diagnosis": diagnosis,
        "target": target,
        "action": None,
        "rationale": None,
        "execution_status": "proposed (Slice 1 — not executed; awaiting Slice 3 sre_apply_fix)",
    }

    target_kind = target.get("kind")

    # The typed-action set is the proposal §7.7.1 closed set. Slice 1+2
    # codify the actions the demo flow needs; the rest land in Slice 3
    # alongside the apply-fix execution path. Slice 1 returns the
    # proposal envelope; the operator applies manually per the runbook.
    if target_kind == "ResourceQuota":
        proposal["action"] = {
            "type": "DeleteResourceQuota",
            "namespace": target.get("namespace"),
            "name": target.get("name"),
        }
        proposal["rationale"] = (
            "Operator-applied ResourceQuotas without the "
            "kars.azure.com/managed-by=controller label are safely deletable "
            "by the SRE agent (per §7.7.1). Removing this quota restores "
            "the namespace's pod admission and the controller will "
            "schedule a fresh sandbox pod."
        )
    elif target_kind in {"Deployment", "StatefulSet", "DaemonSet"} and "image" in (
        _kwargs or {}
    ):
        proposal["action"] = {
            "type": "PatchDeploymentImage",
            "namespace": target.get("namespace"),
            "name": target.get("name"),
            "container": _kwargs.get("container"),
            "image": _kwargs.get("image"),
        }
        proposal["rationale"] = (
            "Patch the container image to the proposed value. The target "
            "namespace must not be in the protected denylist (kars-system, "
            "kars-sre, kube-system, etc. — §7.7.1)."
        )
    elif target_kind in {"Deployment", "StatefulSet"} and "replicas" in (_kwargs or {}):
        proposal["action"] = {
            "type": "ScaleDeployment",
            "namespace": target.get("namespace"),
            "name": target.get("name"),
            "replicas": _kwargs.get("replicas"),
        }
        proposal["rationale"] = "Scale the workload's replica count."
    else:
        # Generic envelope for unknown target kinds — Slice 1 returns
        # the proposal text without a typed action; Slice 3 widens
        # the typed-action set.
        proposal["rationale"] = (
            "No typed action codified yet for this target kind. The "
            "proposal text alone is returned; the operator can apply "
            "manually per the demo runbook."
        )

    return proposal


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
        description=(
            "Return a structured snapshot of every kars-owned CR in every "
            "namespace (KarsSandbox, InferencePolicy, ToolPolicy, "
            "EgressApproval, KarsMemory, KarsEval, TrustGraph, KarsPairing, "
            "A2AAgent, McpServer, KarsAuthConfig). Each CR carries name, "
            "namespace, phase, observedGeneration, lastReconciled, and "
            "conditions. Use this as the first call when starting an "
            "incident investigation."
        ),
        parameters={"type": "object", "properties": {}, "required": []},
        handler=sre_describe_state,
    )

    register_tool(
        name="sre_logs",
        description=(
            "Tail logs from a pod's container via the apiserver. Returns the "
            "last N lines (max 500). Use for diagnosing CrashLoopBackOff or "
            "for inspecting an agent's behaviour."
        ),
        parameters={
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
        description=(
            "Walk the kars-CR health checklist: controller deployment Ready, "
            "every kars CRD installed, no Degraded/Failed sandboxes or "
            "governance CRs, no stale reconciles. Returns a structured "
            "report + a one-line summary suitable for an operator-facing "
            "message."
        ),
        parameters={"type": "object", "properties": {}, "required": []},
        handler=sre_diagnose,
    )

    register_tool(
        name="sre_explain_error",
        description=(
            "Given an error string (pod event reason, controller log line, "
            "etc.), return a root-cause hypothesis from the kars OOTB-blocker "
            "corpus. The hypothesis is a HINT — the agent should then use "
            "the other diagnostic tools to confirm or refute it."
        ),
        parameters={
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
        description=(
            "Return a typed-action proposal for the operator to approve. "
            "READ-ONLY in Slice 1 — Slice 3 adds sre_apply_fix to execute "
            "approved proposals. Use after diagnosing a problem to surface "
            "the recommended remediation."
        ),
        parameters={
            "type": "object",
            "properties": {
                "diagnosis": {
                    "type": "string",
                    "description": "One-line summary of what was diagnosed",
                },
                "target": {
                    "type": "object",
                    "description": "Resource the proposal acts on (kind/namespace/name)",
                    "properties": {
                        "kind": {"type": "string"},
                        "namespace": {"type": "string"},
                        "name": {"type": "string"},
                    },
                },
            },
            "required": ["diagnosis"],
        },
        handler=sre_propose_fix,
    )

    # Slice 2 — register the K8s diagnostic toolset alongside the Slice 1
    # tools. sre_k8s.register() handles its own ctx wiring.
    from . import sre_k8s  # noqa: PLC0415 — lazy import

    sre_k8s.register(ctx)

    logger.info("kars-sre plugin registered (Slice 1: 5 read-only kars-CR tools; Slice 2: 5 K8s diag tools)")
