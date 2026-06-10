# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""kars-sre Hermes plugin — Slice 2 (K8s diagnostic toolset).

Extends the read-only diagnostic surface from kars-CR-centric (Slice 1)
to arbitrary Kubernetes workloads. The tools registered here are the
ones needed to diagnose the Act II ResourceQuota incident end-to-end:

  sre_describe_resource    structured-describe for any k8s resource
                           (Pod / Deployment / Service / Endpoints /
                           EndpointSlice / ResourceQuota / Node /
                           Event), with workload-owner-graph walk for
                           Deployment / StatefulSet / DaemonSet
  sre_what_changed         events of failure-relevant reasons in last
                           N min (default 15) across both core/v1 and
                           events.k8s.io/v1; framing the incident
  sre_endpoints_inspect    Service → selector → matching pods →
                           EndpointSlice subset → endpoint-not-ready
                           reasons (the '0 endpoints' detective tool)
  sre_image_probe          {image} → exists/not + digest + closest
                           in-use tag on this cluster (de-duplicated
                           across workloads)
  sre_top                  metrics.k8s.io wrapper; graceful degrade if
                           metrics-server absent (§7.5 Q4)

Registered alongside the Slice 1 tools by ``sre.register(ctx)`` when
``SRE_ENABLED=true``. The Helm chart's ClusterRole grants the
RBAC required for everything here at install time (Slice 2 is
strictly read-only).

All tools follow the same contract as Slice 1 tools: they NEVER raise
on apiserver errors — those become ``{"error": "..."}`` entries in the
returned dict so the LLM can reason over them.
"""

from __future__ import annotations

import logging
import re
from collections import Counter
from typing import Any
from urllib.parse import quote

import httpx

from . import sre_kube

logger = logging.getLogger("kars.hermes.sre.k8s")


# --------------------------------------------------------------------------
# Apiserver paths
# --------------------------------------------------------------------------

# (kind, plural, api group/version segment)
# api group "" maps to /api/v1; others to /apis/<group>/<version>
RESOURCE_PATHS: dict[str, tuple[str, str]] = {
    "Pod": ("pods", "api/v1"),
    "Service": ("services", "api/v1"),
    "ConfigMap": ("configmaps", "api/v1"),
    "Secret": ("secrets", "api/v1"),
    "Event": ("events", "api/v1"),
    "Node": ("nodes", "api/v1"),
    "Namespace": ("namespaces", "api/v1"),
    "ServiceAccount": ("serviceaccounts", "api/v1"),
    "Endpoints": ("endpoints", "api/v1"),
    "ResourceQuota": ("resourcequotas", "api/v1"),
    "Deployment": ("deployments", "apis/apps/v1"),
    "StatefulSet": ("statefulsets", "apis/apps/v1"),
    "DaemonSet": ("daemonsets", "apis/apps/v1"),
    "ReplicaSet": ("replicasets", "apis/apps/v1"),
    "EndpointSlice": ("endpointslices", "apis/discovery.k8s.io/v1"),
}

# Reasons we treat as "incident-flavoured" — these are the ones
# sre_what_changed surfaces. Sourced from kubelet, scheduler, and
# the controller-managers; intentionally excludes "Normal" reasons
# like Scheduled / Pulled / Started except for ScalingReplicaSet
# (which is what surfaces image/replica edits on Deployments).
WHAT_CHANGED_REASONS: set[str] = {
    "Failed",
    "FailedCreate",
    "FailedDelete",
    "FailedKillPod",
    "FailedMount",
    "FailedScheduling",
    "BackOff",
    "Unhealthy",
    "OOMKilling",
    "Evicted",
    "Preempting",
    "Killing",
    "ScalingReplicaSet",
    "SuccessfulCreate",
    "SuccessfulDelete",
    "DeadlineExceeded",
}


# --------------------------------------------------------------------------
# sre_describe_resource
# --------------------------------------------------------------------------


def _events_for_object(
    kube: sre_kube.KubeClient, namespace: str, kind: str, name: str, limit: int = 25
) -> list[dict[str, Any]]:
    """Fetch recent events targeting a specific object.

    Uses core/v1 events with fieldSelector. The events.k8s.io/v1 events
    have a different shape; we coalesce to a common dict at the call
    site of sre_what_changed instead of here.
    """
    field_selector = (
        f"involvedObject.kind={kind},"
        f"involvedObject.name={name},"
        f"involvedObject.namespace={namespace}"
    )
    try:
        doc = kube.get(
            f"/api/v1/namespaces/{namespace}/events",
            params={"fieldSelector": field_selector, "limit": limit},
        )
        events = []
        for ev in doc.get("items", []):
            events.append(
                {
                    "type": ev.get("type"),
                    "reason": ev.get("reason"),
                    "message": ev.get("message"),
                    "count": ev.get("count"),
                    "firstTimestamp": ev.get("firstTimestamp"),
                    "lastTimestamp": ev.get("lastTimestamp"),
                    "source": (ev.get("source") or {}).get("component"),
                }
            )
        return events
    except Exception as exc:  # noqa: BLE001
        logger.debug("events fetch failed for %s/%s/%s: %s", namespace, kind, name, exc)
        return []


def _summarise_pod(item: dict[str, Any]) -> dict[str, Any]:
    """Reduce a Pod's JSON to the fields the agent cares about."""
    meta = item.get("metadata", {})
    spec = item.get("spec", {})
    status = item.get("status", {})
    containers_summary = []
    for cs in status.get("containerStatuses", []):
        state = cs.get("state", {})
        last_state = cs.get("lastState", {})
        # The waiting reason (ImagePullBackOff, CrashLoopBackOff, etc.)
        # lives at state.waiting.reason; the OOMKill etc. lives at
        # lastState.terminated.reason.
        waiting = state.get("waiting", {}) if state else {}
        terminated_now = state.get("terminated", {}) if state else {}
        terminated_last = last_state.get("terminated", {}) if last_state else {}
        containers_summary.append(
            {
                "name": cs.get("name"),
                "ready": cs.get("ready"),
                "restartCount": cs.get("restartCount"),
                "image": cs.get("image"),
                "imageID": cs.get("imageID"),
                "state": (
                    "waiting" if waiting
                    else "terminated" if terminated_now
                    else "running" if state.get("running")
                    else "unknown"
                ),
                "waitingReason": waiting.get("reason"),
                "waitingMessage": waiting.get("message"),
                "lastTerminatedReason": terminated_last.get("reason"),
                "lastExitCode": terminated_last.get("exitCode"),
            }
        )
    return {
        "kind": "Pod",
        "namespace": meta.get("namespace"),
        "name": meta.get("name"),
        "phase": status.get("phase"),
        "nodeName": spec.get("nodeName"),
        "serviceAccountName": spec.get("serviceAccountName"),
        "imagePullSecrets": [s.get("name") for s in (spec.get("imagePullSecrets") or [])],
        "conditions": [
            {"type": c.get("type"), "status": c.get("status"), "reason": c.get("reason"), "message": c.get("message")}
            for c in (status.get("conditions") or [])
        ],
        "containers": containers_summary,
        "ownerReferences": [
            {"kind": o.get("kind"), "name": o.get("name")}
            for o in (meta.get("ownerReferences") or [])
        ],
    }


def _summarise_workload(item: dict[str, Any]) -> dict[str, Any]:
    """Reduce a Deployment / StatefulSet / DaemonSet / ReplicaSet."""
    meta = item.get("metadata", {})
    spec = item.get("spec", {})
    status = item.get("status", {})
    template = spec.get("template", {}).get("spec", {})
    containers = [
        {
            "name": c.get("name"),
            "image": c.get("image"),
            "resources": c.get("resources"),
        }
        for c in (template.get("containers") or [])
    ]
    return {
        "kind": item.get("kind", "Workload"),
        "namespace": meta.get("namespace"),
        "name": meta.get("name"),
        "generation": meta.get("generation"),
        "observedGeneration": status.get("observedGeneration"),
        "replicas": status.get("replicas"),
        "readyReplicas": status.get("readyReplicas"),
        "availableReplicas": status.get("availableReplicas"),
        "selector": spec.get("selector"),
        "containers": containers,
        "ownerReferences": [
            {"kind": o.get("kind"), "name": o.get("name")}
            for o in (meta.get("ownerReferences") or [])
        ],
        "conditions": [
            {"type": c.get("type"), "status": c.get("status"), "reason": c.get("reason"), "message": c.get("message")}
            for c in (status.get("conditions") or [])
        ],
    }


def _summarise_service(item: dict[str, Any]) -> dict[str, Any]:
    meta = item.get("metadata", {})
    spec = item.get("spec", {})
    return {
        "kind": "Service",
        "namespace": meta.get("namespace"),
        "name": meta.get("name"),
        "type": spec.get("type"),
        "selector": spec.get("selector"),
        "ports": spec.get("ports"),
        "clusterIP": spec.get("clusterIP"),
    }


def _summarise_resource_quota(item: dict[str, Any]) -> dict[str, Any]:
    meta = item.get("metadata", {})
    spec = item.get("spec", {})
    status = item.get("status", {})
    return {
        "kind": "ResourceQuota",
        "namespace": meta.get("namespace"),
        "name": meta.get("name"),
        "labels": meta.get("labels"),
        "hard": spec.get("hard"),
        "usedHard": status.get("hard"),
        "used": status.get("used"),
        # NOTE: The label `kars.azure.com/managed-by` is what gates
        # whether the SRE agent's DeleteResourceQuota typed action
        # (§7.7.1) is permitted on this resource. Surfacing it here
        # lets the agent reason about whether a proposed delete is
        # safe BEFORE proposing it.
        "isKarsManaged": (meta.get("labels") or {}).get("kars.azure.com/managed-by") == "controller",
    }


def _walk_owner_graph(
    kube: sre_kube.KubeClient, kind: str, namespace: str, name: str
) -> dict[str, Any]:
    """For a Deployment/StatefulSet/DaemonSet, walk down to pods + events.

    Returns:
        {
          "workload": {...summarised...},
          "replica_sets": [...],  # only for Deployment
          "pods": [...],
          "events_on_workload": [...],
          "events_on_replica_sets": [...],
          "events_on_pods": [...],
        }
    """
    out: dict[str, Any] = {}
    plural, api_seg = RESOURCE_PATHS[kind]

    # 1) The workload itself
    try:
        wl = kube.get(f"/{api_seg}/namespaces/{namespace}/{plural}/{name}")
        wl["kind"] = kind  # ensure kind is populated on items fetched by-name
        out["workload"] = _summarise_workload(wl)
    except httpx.HTTPStatusError as exc:
        out["workload"] = {"error": f"{exc.response.status_code} {exc.response.reason_phrase}"}
        return out
    except Exception as exc:  # noqa: BLE001
        out["workload"] = {"error": str(exc)}
        return out

    # 2) For Deployments, walk through ReplicaSets
    selector = (wl.get("spec") or {}).get("selector") or {}
    match_labels = selector.get("matchLabels") or {}
    label_selector = ",".join(f"{k}={v}" for k, v in match_labels.items())

    if kind == "Deployment" and label_selector:
        try:
            rs_doc = kube.get(
                f"/apis/apps/v1/namespaces/{namespace}/replicasets",
                params={"labelSelector": label_selector},
            )
            out["replica_sets"] = [
                _summarise_workload({**rs, "kind": "ReplicaSet"})
                for rs in rs_doc.get("items", [])
            ]
        except Exception as exc:  # noqa: BLE001
            out["replica_sets"] = {"error": str(exc)}

    # 3) Pods matching the selector
    out["pods"] = []
    if label_selector:
        try:
            pod_doc = kube.get(
                f"/api/v1/namespaces/{namespace}/pods",
                params={"labelSelector": label_selector},
            )
            out["pods"] = [_summarise_pod(p) for p in pod_doc.get("items", [])]
        except Exception as exc:  # noqa: BLE001
            out["pods"] = {"error": str(exc)}

    # 4) Events on the workload + replica sets + pods (helps the agent
    # spot 'exceeded quota' on the RS, not just on the workload)
    out["events_on_workload"] = _events_for_object(kube, namespace, kind, name)
    if isinstance(out.get("replica_sets"), list):
        rs_events = []
        for rs in out["replica_sets"]:
            rs_events.extend(
                _events_for_object(kube, namespace, "ReplicaSet", rs["name"])
            )
        out["events_on_replica_sets"] = rs_events
    if isinstance(out.get("pods"), list):
        pod_events = []
        for pod in out["pods"]:
            pod_events.extend(
                _events_for_object(kube, namespace, "Pod", pod["name"])
            )
        out["events_on_pods"] = pod_events

    return out


def _impl_sre_describe_resource(
    *,
    kind: str,
    namespace: str | None = None,
    name: str,
    **_kwargs: Any,
) -> dict[str, Any]:
    """Tool: structured-describe for any K8s resource.

    For Pod / Service / ResourceQuota / ConfigMap etc. — returns a
    structured summary + recent events on the object.

    For Deployment / StatefulSet / DaemonSet — walks the workload
    owner graph: workload → ReplicaSets (for Deployments) → matching
    Pods → events on every level. This is THE diagnostic shortcut
    for incidents like ImagePullBackOff, exceeded-quota,
    CrashLoopBackOff — one tool call returns the whole picture.

    Args:
        kind: K8s kind, e.g. "Pod", "Deployment", "ResourceQuota".
        namespace: namespace (required for namespaced kinds).
        name: resource name.
    """
    if kind not in RESOURCE_PATHS:
        return {
            "error": f"unknown kind: {kind}",
            "supported_kinds": sorted(RESOURCE_PATHS.keys()),
        }

    # Owner-graph walk for workload kinds
    if kind in {"Deployment", "StatefulSet", "DaemonSet"}:
        if not namespace:
            return {"error": f"{kind} is namespaced — provide namespace"}
        return _walk_owner_graph(sre_kube.client(), kind, namespace, name)

    # Direct describe for other kinds
    plural, api_seg = RESOURCE_PATHS[kind]
    if namespace:
        path = f"/{api_seg}/namespaces/{namespace}/{plural}/{name}"
    else:
        path = f"/{api_seg}/{plural}/{name}"
    kube = sre_kube.client()
    try:
        item = kube.get(path)
        item["kind"] = kind  # ensure populated
    except httpx.HTTPStatusError as exc:
        return {
            "kind": kind,
            "name": name,
            "namespace": namespace,
            "error": f"{exc.response.status_code} {exc.response.reason_phrase}",
        }
    except Exception as exc:  # noqa: BLE001
        return {"kind": kind, "name": name, "namespace": namespace, "error": str(exc)}

    summariser = {
        "Pod": _summarise_pod,
        "Deployment": _summarise_workload,
        "StatefulSet": _summarise_workload,
        "DaemonSet": _summarise_workload,
        "ReplicaSet": _summarise_workload,
        "Service": _summarise_service,
        "ResourceQuota": _summarise_resource_quota,
    }.get(kind)

    summary: dict[str, Any]
    if summariser:
        summary = summariser(item)
    else:
        # Generic fallback for ConfigMap / Secret / Node / etc.
        meta = item.get("metadata", {})
        summary = {
            "kind": kind,
            "namespace": meta.get("namespace"),
            "name": meta.get("name"),
            "labels": meta.get("labels"),
            "annotations": meta.get("annotations"),
            "creationTimestamp": meta.get("creationTimestamp"),
        }
        # Type-specific fields
        if kind == "ConfigMap":
            summary["data_keys"] = list((item.get("data") or {}).keys())
        elif kind == "Secret":
            # NEVER include .data — strip per §6.4 (router proxy also
            # strips, but defense in depth at the plugin layer too).
            summary["type"] = item.get("type")
            summary["data_keys"] = list((item.get("data") or {}).keys())
        elif kind == "Node":
            summary["unschedulable"] = (item.get("spec") or {}).get("unschedulable", False)
            summary["taints"] = (item.get("spec") or {}).get("taints", [])
            summary["conditions"] = [
                {"type": c.get("type"), "status": c.get("status"), "reason": c.get("reason")}
                for c in ((item.get("status") or {}).get("conditions") or [])
            ]

    # Add events on the resource (namespaced kinds only)
    if namespace:
        summary["recent_events"] = _events_for_object(kube, namespace, kind, name)

    return summary


# --------------------------------------------------------------------------
# sre_what_changed
# --------------------------------------------------------------------------


def _impl_sre_what_changed(
    *,
    namespace: str | None = None,
    minutes: int = 15,
    **_kwargs: Any,
) -> dict[str, Any]:
    """Tool: events of failure-relevant reasons in the last N minutes.

    Surfaces events from BOTH ``core/v1/events`` (older API) and
    ``events.k8s.io/v1/events`` (newer API) — they have different
    retention windows and shapes; the agent should not have to know
    which is in play.

    Args:
        namespace: limit to one namespace (omit for cluster-wide).
        minutes: lookback window (default 15, capped at 60).

    Returns:
        {
          "since_minutes": N,
          "namespace": "..." or "*",
          "events_core": [...],
          "events_new":  [...],
        }
    """
    minutes = max(1, min(minutes, 60))
    kube = sre_kube.client()

    out: dict[str, Any] = {
        "since_minutes": minutes,
        "namespace": namespace or "*",
        "events_core": [],
        "events_new": [],
    }

    # core/v1/events
    if namespace:
        core_path = f"/api/v1/namespaces/{namespace}/events"
    else:
        core_path = "/api/v1/events"
    try:
        doc = kube.get(core_path, params={"limit": 200})
        for ev in doc.get("items", []):
            reason = ev.get("reason")
            if reason in WHAT_CHANGED_REASONS:
                out["events_core"].append(
                    {
                        "namespace": (ev.get("involvedObject") or {}).get("namespace"),
                        "kind": (ev.get("involvedObject") or {}).get("kind"),
                        "name": (ev.get("involvedObject") or {}).get("name"),
                        "type": ev.get("type"),
                        "reason": reason,
                        "message": ev.get("message"),
                        "count": ev.get("count"),
                        "lastTimestamp": ev.get("lastTimestamp"),
                    }
                )
    except Exception as exc:  # noqa: BLE001
        out["events_core"] = {"error": str(exc)}

    # events.k8s.io/v1/events
    if namespace:
        new_path = f"/apis/events.k8s.io/v1/namespaces/{namespace}/events"
    else:
        new_path = "/apis/events.k8s.io/v1/events"
    try:
        doc = kube.get(new_path, params={"limit": 200})
        for ev in doc.get("items", []):
            reason = ev.get("reason")
            if reason in WHAT_CHANGED_REASONS:
                regarding = ev.get("regarding") or {}
                out["events_new"].append(
                    {
                        "namespace": regarding.get("namespace"),
                        "kind": regarding.get("kind"),
                        "name": regarding.get("name"),
                        "type": ev.get("type"),
                        "reason": reason,
                        "note": ev.get("note"),
                        "deprecatedCount": ev.get("deprecatedCount"),
                        "eventTime": ev.get("eventTime"),
                    }
                )
    except Exception as exc:  # noqa: BLE001
        out["events_new"] = {"error": str(exc)}

    return out


# --------------------------------------------------------------------------
# sre_endpoints_inspect
# --------------------------------------------------------------------------


def _impl_sre_endpoints_inspect(
    *,
    namespace: str,
    service: str,
    **_kwargs: Any,
) -> dict[str, Any]:
    """Tool: Service → selector → matching pods → EndpointSlice readiness.

    The "0 endpoints" detective tool. Answers: why isn't this Service
    routing traffic? Walks:

      1. Fetch Service spec, capture its selector
      2. List Pods matching the selector
      3. List EndpointSlices in the namespace owned by the Service
      4. Surface the diff: pods that match the selector but are not
         in any EndpointSlice subset (suggests readiness-probe
         failures), and the EndpointSlice's not-ready conditions for
         each endpoint.
    """
    kube = sre_kube.client()
    out: dict[str, Any] = {"namespace": namespace, "service": service}

    # 1) Service
    try:
        svc = kube.get(f"/api/v1/namespaces/{namespace}/services/{service}")
    except httpx.HTTPStatusError as exc:
        return {**out, "error": f"{exc.response.status_code} {exc.response.reason_phrase}"}
    except Exception as exc:  # noqa: BLE001
        return {**out, "error": str(exc)}

    selector = (svc.get("spec") or {}).get("selector") or {}
    out["selector"] = selector
    out["service_type"] = (svc.get("spec") or {}).get("type")
    if not selector:
        out["finding"] = (
            "Service has no selector — endpoints are managed externally "
            "(or via the headless / ExternalName pattern). No further "
            "diagnosis from this tool."
        )
        return out

    # 2) Pods matching the selector
    label_selector = ",".join(f"{k}={v}" for k, v in selector.items())
    try:
        pod_doc = kube.get(
            f"/api/v1/namespaces/{namespace}/pods",
            params={"labelSelector": label_selector},
        )
        out["matching_pods"] = [
            {
                "name": p.get("metadata", {}).get("name"),
                "phase": (p.get("status") or {}).get("phase"),
                "podIP": (p.get("status") or {}).get("podIP"),
                "ready": all(
                    c.get("status") == "True"
                    for c in ((p.get("status") or {}).get("conditions") or [])
                    if c.get("type") == "Ready"
                ),
            }
            for p in pod_doc.get("items", [])
        ]
    except Exception as exc:  # noqa: BLE001
        out["matching_pods"] = {"error": str(exc)}

    # 3) EndpointSlices owned by the service
    try:
        es_doc = kube.get(
            f"/apis/discovery.k8s.io/v1/namespaces/{namespace}/endpointslices",
            params={"labelSelector": f"kubernetes.io/service-name={service}"},
        )
        slices = []
        for es in es_doc.get("items", []):
            endpoints = []
            for ep in es.get("endpoints", []):
                endpoints.append(
                    {
                        "addresses": ep.get("addresses"),
                        "conditions": ep.get("conditions"),
                        "targetRef": ep.get("targetRef"),
                    }
                )
            slices.append(
                {
                    "name": es.get("metadata", {}).get("name"),
                    "addressType": es.get("addressType"),
                    "endpoints": endpoints,
                }
            )
        out["endpoint_slices"] = slices
    except Exception as exc:  # noqa: BLE001
        out["endpoint_slices"] = {"error": str(exc)}

    # 4) Synthesise a finding
    n_pods = len(out.get("matching_pods", [])) if isinstance(out.get("matching_pods"), list) else 0
    n_ready = sum(
        1 for p in (out.get("matching_pods") or []) if isinstance(p, dict) and p.get("ready")
    )
    n_endpoints = 0
    if isinstance(out.get("endpoint_slices"), list):
        for es in out["endpoint_slices"]:
            for ep in es.get("endpoints", []):
                if (ep.get("conditions") or {}).get("ready"):
                    n_endpoints += sum(1 for _ in (ep.get("addresses") or []))

    if n_pods == 0:
        out["finding"] = (
            "No pods match the service's selector. Either the workload "
            "isn't deployed, or its labels were changed to not match. "
            "Check the controlling Deployment/StatefulSet for the "
            "current pod-template labels."
        )
    elif n_ready == 0 and n_pods > 0:
        out["finding"] = (
            f"{n_pods} pod(s) match the selector but none are Ready. "
            "Likely cause: readiness probe failing, container startup "
            "error, or workload-config bug. Use sre_describe_resource "
            "on the pods + sre_logs to find the root cause."
        )
    elif n_endpoints == 0:
        out["finding"] = (
            f"{n_ready}/{n_pods} pod(s) are Ready but the EndpointSlice "
            "has zero ready addresses. Likely cause: the Service's "
            "targetPort doesn't match any container port on the pods, "
            "or the EndpointSlice controller is lagging."
        )
    else:
        out["finding"] = (
            f"{n_endpoints} endpoint(s) ready across "
            f"{len(out.get('endpoint_slices', []))} slice(s). Service "
            "should be routing traffic."
        )
    return out


# --------------------------------------------------------------------------
# sre_image_probe
# --------------------------------------------------------------------------


_IMAGE_RE = re.compile(
    r"^(?P<registry>[a-z0-9.\-]+(?::\d+)?/)?"
    r"(?P<repo>[a-z0-9._/\-]+?)"
    r"(?::(?P<tag>[A-Za-z0-9_.\-]+))?"
    r"(?:@(?P<digest>sha256:[a-f0-9]+))?$"
)


def _parse_image(image: str) -> dict[str, str | None]:
    m = _IMAGE_RE.match(image.strip())
    if not m:
        return {"registry": None, "repo": image, "tag": None, "digest": None}
    parts: dict[str, str | None] = {**m.groupdict()}
    if parts.get("registry"):
        parts["registry"] = parts["registry"].rstrip("/")
    return parts


def _all_images_in_use(kube: sre_kube.KubeClient) -> Counter[str]:
    """Return a Counter of every container image observed on the cluster.

    Walks Pods cluster-wide. Used by ``sre_image_probe`` to surface
    the "closest tag in use on this cluster" suggestion when an
    operator's image string doesn't exist.
    """
    counts: Counter[str] = Counter()
    try:
        doc = kube.get("/api/v1/pods", params={"limit": 500})
        for p in doc.get("items", []):
            for c in (p.get("spec") or {}).get("containers") or []:
                img = c.get("image")
                if img:
                    counts[img] += 1
            for c in (p.get("spec") or {}).get("initContainers") or []:
                img = c.get("image")
                if img:
                    counts[img] += 1
    except Exception as exc:  # noqa: BLE001
        logger.debug("could not enumerate cluster images: %s", exc)
    return counts


def _edit_distance(a: str, b: str) -> int:
    """Levenshtein distance — small, ~30-LOC pure-python implementation
    sufficient for our 'closest tag' suggestion (image tags are short)."""
    if a == b:
        return 0
    if len(a) < len(b):
        a, b = b, a
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        curr = [i] + [0] * len(b)
        for j, cb in enumerate(b, 1):
            curr[j] = min(
                prev[j] + 1,        # delete
                curr[j - 1] + 1,    # insert
                prev[j - 1] + (ca != cb),  # substitute
            )
        prev = curr
    return prev[-1]


def _impl_sre_image_probe(*, image: str, **_kwargs: Any) -> dict[str, Any]:
    """Tool: probe an image reference and suggest closest in-use tags.

    Slice 2 implementation: does NOT actually reach out to a registry
    (that requires registry-auth plumbing per registry, which lands in
    Slice 4+). Instead, it answers the question that's actually most
    useful in incidents — "what tags of this repo are in use on this
    cluster RIGHT NOW?" — by enumerating Pods.

    Returns:
        {
          "image": <input>,
          "parsed": {registry, repo, tag, digest},
          "in_use_on_cluster": [{image, count}, ...],
          "closest_in_use": <image> | None,
          "advice": <string>,
        }
    """
    parsed = _parse_image(image)
    kube = sre_kube.client()

    all_images = _all_images_in_use(kube)

    # Find images that share the same repo prefix
    repo = parsed.get("repo") or ""
    same_repo: list[tuple[str, int]] = []
    for img, count in all_images.items():
        p = _parse_image(img)
        if p.get("repo") == repo and (
            parsed.get("registry") is None or p.get("registry") == parsed.get("registry")
        ):
            same_repo.append((img, count))
    same_repo.sort(key=lambda t: t[1], reverse=True)

    # Closest tag by edit distance against the requested tag
    closest: str | None = None
    if parsed.get("tag") and same_repo:
        best_dist = 10**9
        for img, _count in same_repo:
            p = _parse_image(img)
            if p.get("tag"):
                d = _edit_distance(parsed["tag"], p["tag"])  # type: ignore[arg-type]
                if d < best_dist:
                    best_dist = d
                    closest = img

    advice: str
    if not same_repo:
        advice = (
            f"No pod on this cluster currently uses the repo {repo!r}. The "
            "image may not exist, or this is the first deployment of it. "
            "Slice 4+ adds a real registry probe to confirm; for now, "
            "verify the registry / repo path is spelled correctly."
        )
    elif closest and closest != image:
        advice = (
            f"Image {image!r} is not currently used on this cluster, but "
            f"{closest!r} is (running in {dict(same_repo).get(closest, 0)} "
            "pod(s)). If the failing image string contains a typo, this is "
            "the closest match by edit-distance."
        )
    else:
        advice = (
            f"Image {image!r} matches an image currently in use on the "
            "cluster. The failure is likely registry-side (auth, throttle, "
            "outage) rather than a typo."
        )

    return {
        "image": image,
        "parsed": parsed,
        "in_use_on_cluster": [{"image": img, "count": count} for img, count in same_repo[:10]],
        "closest_in_use": closest,
        "advice": advice,
    }


# --------------------------------------------------------------------------
# sre_top
# --------------------------------------------------------------------------


def _impl_sre_top(
    *,
    scope: str = "pods",
    namespace: str | None = None,
    **_kwargs: Any,
) -> dict[str, Any]:
    """Tool: metrics.k8s.io wrapper for pod / node CPU + memory.

    Args:
        scope: "pods" or "nodes".
        namespace: required for scope=pods if filtering to one ns.

    Returns ``{"unavailable": "..."}`` when metrics-server is absent
    (the agent's planner routes around it per §7.5 Q4).
    """
    kube = sre_kube.client()
    if scope == "nodes":
        path = "/apis/metrics.k8s.io/v1beta1/nodes"
    elif scope == "pods":
        if namespace:
            path = f"/apis/metrics.k8s.io/v1beta1/namespaces/{quote(namespace)}/pods"
        else:
            path = "/apis/metrics.k8s.io/v1beta1/pods"
    else:
        return {"error": f"unknown scope: {scope}", "valid_scopes": ["pods", "nodes"]}

    try:
        doc = kube.get(path)
    except httpx.HTTPStatusError as exc:
        # 404 = metrics-server not registered as an APIService.
        if exc.response.status_code == 404:
            return {
                "unavailable": "metrics-server is not installed on this cluster.",
                "scope": scope,
            }
        return {"error": f"{exc.response.status_code} {exc.response.reason_phrase}"}
    except Exception as exc:  # noqa: BLE001
        return {"error": str(exc)}

    items = []
    for it in doc.get("items", []):
        meta = it.get("metadata", {})
        if scope == "nodes":
            usage = it.get("usage") or {}
            items.append(
                {
                    "name": meta.get("name"),
                    "cpu": usage.get("cpu"),
                    "memory": usage.get("memory"),
                    "timestamp": it.get("timestamp"),
                }
            )
        else:
            containers = [
                {
                    "name": c.get("name"),
                    "cpu": (c.get("usage") or {}).get("cpu"),
                    "memory": (c.get("usage") or {}).get("memory"),
                }
                for c in (it.get("containers") or [])
            ]
            items.append(
                {
                    "namespace": meta.get("namespace"),
                    "name": meta.get("name"),
                    "containers": containers,
                    "timestamp": it.get("timestamp"),
                }
            )
    return {"scope": scope, "items": items}


# --------------------------------------------------------------------------
# Plugin registration
# --------------------------------------------------------------------------


def register(ctx: Any) -> None:  # noqa: ANN401 — Hermes' ctx is dynamic
    """Register the Slice 2 K8s diagnostic tools.

    Called from ``sre.register()`` alongside the Slice 1 tools when
    ``SRE_ENABLED=true``.
    """
    register_tool = getattr(ctx, "register_tool", None)
    if not callable(register_tool):
        logger.warning("Hermes ctx has no register_tool — Slice 2 SRE tools not registered")
        return

    register_tool(
        name="sre_describe_resource",
        toolset="sre",
        description=(
            "Structured-describe for any K8s resource (Pod, Deployment, "
            "Service, ResourceQuota, ConfigMap, Secret metadata only, "
            "EndpointSlice, Node, Event, etc.). For workload kinds "
            "(Deployment, StatefulSet, DaemonSet) walks the owner graph: "
            "workload → ReplicaSet → Pods → events on every level. This "
            "is THE single-call diagnostic for most workload incidents."
        ),
        schema={
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "description": "K8s kind, e.g. Pod, Deployment, ResourceQuota",
                },
                "namespace": {
                    "type": "string",
                    "description": "Namespace (required for namespaced kinds)",
                },
                "name": {"type": "string", "description": "Resource name"},
            },
            "required": ["kind", "name"],
        },
        handler=sre_describe_resource,
    )

    register_tool(
        name="sre_what_changed",
        toolset="sre",
        description=(
            "Events of failure-relevant reasons in the last N minutes "
            "across core/v1 + events.k8s.io/v1. Use FIRST in an incident "
            "to frame the time-window: what broke when?"
        ),
        schema={
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "Limit to one namespace; omit for cluster-wide",
                },
                "minutes": {
                    "type": "integer",
                    "description": "Lookback window in minutes (1-60, default 15)",
                    "default": 15,
                },
            },
            "required": [],
        },
        handler=sre_what_changed,
    )

    register_tool(
        name="sre_endpoints_inspect",
        toolset="sre",
        description=(
            "Service → selector → matching pods → EndpointSlice readiness. "
            "Diagnoses 'service has no endpoints' incidents: are there pods "
            "matching the selector? are they Ready? are they in the "
            "EndpointSlice? Returns a finding summary the agent can quote."
        ),
        schema={
            "type": "object",
            "properties": {
                "namespace": {"type": "string"},
                "service": {"type": "string"},
            },
            "required": ["namespace", "service"],
        },
        handler=sre_endpoints_inspect,
    )

    register_tool(
        name="sre_image_probe",
        toolset="sre",
        description=(
            "Given an image reference, return: (a) what tags of the same "
            "repo are CURRENTLY IN USE on this cluster, (b) the closest "
            "match by edit-distance to the requested tag. Use after "
            "sre_describe_resource shows ImagePullBackOff."
        ),
        schema={
            "type": "object",
            "properties": {
                "image": {
                    "type": "string",
                    "description": "Image reference, e.g. 'nginx:1.27.3'",
                },
            },
            "required": ["image"],
        },
        handler=sre_image_probe,
    )

    register_tool(
        name="sre_top",
        toolset="sre",
        description=(
            "CPU + memory usage per pod or per node (metrics.k8s.io). "
            "Returns {unavailable: 'metrics-server not installed'} if "
            "the metrics API isn't registered — the agent's planner "
            "routes around it."
        ),
        schema={
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["pods", "nodes"],
                    "default": "pods",
                },
                "namespace": {
                    "type": "string",
                    "description": "Required for scope=pods; omit for cluster-wide",
                },
            },
            "required": [],
        },
        handler=sre_top,
    )

    logger.info("kars-sre Slice 2 (K8s diagnostic toolset) registered — 5 tools")


# ─── Hermes-shape adapters ────────────────────────────────────────────
# Hermes invokes tool handlers as `handler(args: dict, **ctx)`. Our
# impl functions take **kwargs so they're easy to unit-test; these
# adapters bridge the two surfaces.

def sre_image_probe(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_image_probe(**args)

def sre_what_changed(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_what_changed(**args)

def sre_describe_resource(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_describe_resource(**args)

def sre_top(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_top(**args)

def sre_endpoints_inspect(args=None, **_ctx):  # noqa: ANN001 — Hermes call shape
    if args is None:
        args = {}
    return _impl_sre_endpoints_inspect(**args)
