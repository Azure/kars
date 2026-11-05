# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json
import re

STAGES = {
    "registrar", "controller-review", "release-inventory", "prerequisite-chart-render",
    "action-schema-review", "helm-compatibility", "action-schema-migration",
    "core-schema-preparation", "helm-server-dry-run", "helm-upgrade",
    "template-ownership-review", "schema-publication", "template-authority-write",
    "controller-rollout",
}
REASONS = {
    "ContainerCreating", "PodInitializing", "ImagePullBackOff", "ErrImagePull",
    "InvalidImageName", "CrashLoopBackOff", "CreateContainerConfigError",
    "CreateContainerError", "RunContainerError", "OOMKilled", "Error", "Completed",
    "Unschedulable", "ContainersNotReady", "PodCompleted", "Evicted",
}


def last_stage(stderr):
    lines = [line for line in stderr.splitlines() if line.startswith("SRE-STAGE-PROGRESS ")]
    if not lines:
        return "unclassified"
    values = [line.removeprefix("SRE-STAGE-PROGRESS ") for line in lines]
    return values[-1] if all(value in STAGES for value in values) else "unclassified"


def pod_facts(pod):
    status = pod.get("status", {})
    phase = status.get("phase")
    name = pod.get("metadata", {}).get("name", "")
    result = {"name": name if isinstance(name, str) and re.fullmatch(r"[a-z0-9][a-z0-9.-]{0,252}", name) else "unclassified",
              "phase": phase if phase in {"Pending", "Running", "Succeeded", "Failed", "Unknown"} else "Unknown",
              "containers": [], "conditions": []}
    for condition in status.get("conditions", []):
        if condition.get("type") in {"PodScheduled", "Initialized", "ContainersReady", "Ready"}:
            value = condition.get("status")
            result["conditions"].append({"type": condition["type"],
                "status": value if value in {"True", "False", "Unknown"} else "Unknown",
                "reason": condition.get("reason") if condition.get("reason") in REASONS else "unclassified"})
    for container in status.get("initContainerStatuses", []) + status.get("containerStatuses", []):
        state = container.get("state", {})
        states = [kind for kind in ("waiting", "running", "terminated") if kind in state]
        kind = states[0] if len(states) == 1 else "unclassified"
        detail = state.get(kind, {})
        count, code = container.get("restartCount"), detail.get("exitCode")
        result["containers"].append({"ready": container.get("ready") is True, "state": kind,
            "restarts": count if type(count) is int and count >= 0 else None,
            "exitCode": code if type(code) is int and 0 <= code <= 255 else None,
            "reason": detail.get("reason") if detail.get("reason") in REASONS else "unclassified"})
    return result


def collect_stage_workloads(h):
    # These built-ins participate in Helm's waiter but are outside kars-sre.
    # Run before the disposable cluster is torn down; never include logs/messages.
    for namespace in ("kars-system", "headlamp"):
        try:
            pods = json.loads(h.k("get", "pods", "-n", namespace, "-o", "json", timeout=10))["items"]
            print("SRE-STAGE-WORKLOADS", json.dumps({"namespace": namespace,
                "pods": [pod_facts(pod) for pod in pods]}), flush=True)
        except Exception:
            print("SRE-STAGE-WORKLOADS", json.dumps({"namespace": namespace, "unavailable": True}), flush=True)
