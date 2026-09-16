# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Read-only failure evidence, including unready routers; never authorizes a probe."""

import json
import re
from urllib.parse import urlencode

from native_api import CORE, command, core, require, resource
from observation_diagnostics import READ_ERRORS, VERSION, identity, owner
from observer_network_diagnostics import complete_inventory

UNAVAILABLE = "Native rollout diagnostic unavailable"
REASONS = frozenset("""
Completed Error OOMKilled ContainerCannotRun StartError CrashLoopBackOff
ImagePullBackOff ErrImagePull CreateContainerConfigError CreateContainerError
RunContainerError ContainerCreating PodInitializing
""".split())
STARTUP = {
    ("kars_inference_router", "kars Inference Router starting"): "starting",
    ("kars_inference_router", "Registry topology"): "configuration-loaded",
    ("kars_inference_router", "Listening on 0.0.0.0:8443"): "listener-planned",
    ("kars_inference_router::service_observation_tls",
     "Private observation listener stopped"): "observation-listener-stopped",
}


def bounded_integer(value, maximum):
    require(type(value) is int and 0 <= value <= maximum, UNAVAILABLE)
    return value


def container_state(value):
    require(isinstance(value, dict) and len(value) <= 1, UNAVAILABLE)
    if not value:
        return {"phase": "absent"}
    phase = next(iter(value))
    require(phase in ("running", "waiting", "terminated")
            and isinstance(value[phase], dict), UNAVAILABLE)
    state = value[phase]
    result = {"phase": phase}
    if phase != "running":
        result["reason"] = state.get("reason") if state.get("reason") in REASONS else "Other"
    if phase == "terminated":
        result["exitCode"] = bounded_integer(state.get("exitCode"), 255)
        if "signal" in state:
            result["signal"] = bounded_integer(state["signal"], 64)
    return result


def startup_records(raw):
    require(isinstance(raw, str) and len(raw.encode()) <= 131072, UNAVAILABLE)
    records = []
    error_records = 0
    for line in raw.splitlines()[-512:]:
        try:
            value = json.loads(line)
        except (ValueError, TypeError):
            continue
        if not isinstance(value, dict) or not isinstance(value.get("fields"), dict):
            continue
        target, message = value.get("target"), value["fields"].get("message")
        if not isinstance(target, str) or not isinstance(message, str):
            continue
        stage = STARTUP.get((target, message))
        if stage and (not records or records[-1] != stage):
            records.append(stage)
        if (target == "kars_inference_router" or target.startswith("kars_inference_router::")) \
                and value.get("level") == "ERROR":
            error_records += 1
    return {"stages": records[-16:], "errorRecords": error_records,
            "coverage": "bounded-process-log-tail",
            "listenerReachabilityProved": False}


def read_logs(namespace, name, previous=False):
    try:
        raw = command("kubectl", "logs", "-n", namespace, name, "-c", "inference-router",
                      "--tail=512", "--limit-bytes=131072", "--request-timeout=10s",
                      *(["--previous"] if previous else []), timeout=15)
        return {"available": True, **startup_records(raw)}
    except READ_ERRORS:
        return {"available": False, "category": "log-read-unavailable"}


def event_records(events, namespace, name, pod_uid):
    records = []
    for event in complete_inventory(events, 128):
        require(isinstance(event, dict), UNAVAILABLE)
        ref, source = event.get("involvedObject", {}), event.get("source", {})
        require(isinstance(ref, dict) and isinstance(source, dict), UNAVAILABLE)
        if (ref.get("kind") != "Pod" or ref.get("apiVersion") != "v1"
                or ref.get("namespace") != namespace or ref.get("name") != name
                or ref.get("uid") != pod_uid
                or ref.get("fieldPath") != "spec.containers{inference-router}"
                or source.get("component") != "kubelet"):
            continue
        message = event.get("message")
        if event.get("reason") != "Unhealthy" or not isinstance(message, str):
            continue
        match = re.fullmatch(r"(Readiness|Liveness|Startup) probe (failed|errored): ([\s\S]*)", message)
        if match is None:
            continue
        probe, outcome, detail = match.groups()
        category = "other"
        code = re.fullmatch(r"HTTP probe failed with statuscode: ([1-5][0-9]{2})", detail)
        if code:
            category = "http-response"
        elif "connection refused" in detail:
            category = "connection-refused"
        elif any(text in detail for text in (
                "context deadline exceeded", "Client.Timeout", "i/o timeout")):
            category = "timeout"
        record = {"probe": probe.lower(), "outcome": outcome, "category": category,
                  "httpStatus": int(code[1]) if code else 0}
        if record not in records:
            records.append(record)
    return {"coverage": "pod-lifetime-events-not-current-process",
            "records": records[:24]}


def collect(setup, target):
    result = {"diagnosticOnly": True, "available": False, "category": "target-unavailable"}
    anchors = {}
    try:
        require(isinstance(target, dict) and target.get("workspace") == CORE
                and isinstance(target.get("uid"), str) and target["uid"]
                and isinstance(target.get("sandbox"), str)
                and re.fullmatch(r"[a-z0-9][-a-z0-9]{0,62}", target["sandbox"]), UNAVAILABLE)
        name, namespace = target["sandbox"], "kars-" + target["sandbox"]

        def read(path):
            value = setup.admin.get(path)
            identity(value)
            anchors[path] = value
            return value

        result["category"] = "authority-unavailable"
        sandbox = read(resource(CORE, "karssandboxes", name))
        observed = sandbox.get("status", {}).get("serviceObservation", {})
        require(identity(sandbox)[0] == target["uid"] and isinstance(observed, dict)
                and sandbox["metadata"].get("name") == name
                and sandbox["metadata"].get("namespace") == CORE
                and observed.get("phase") in ("Prepared", "Ready")
                and isinstance(observed.get("version"), str) and observed["version"], UNAVAILABLE)
        ns = read("/api/v1/namespaces/" + namespace)
        deployment = read(resource(namespace, "deployments", name, "/apis/apps/v1"))
        require(ns["metadata"].get("name") == namespace
                and identity(ns)[0] == observed.get("namespaceUid")
                and deployment["metadata"].get("namespace") == namespace
                and deployment["metadata"].get("name") == name
                and identity(deployment)[0] == observed.get("deploymentUid"), UNAVAILABLE)
        template = deployment["spec"]["template"]
        require(template["spec"].get("serviceAccountName") == "sandbox"
                and template["metadata"]["labels"].get("kars.azure.com/sandbox") == name, UNAVAILABLE)
        result["category"] = "consumer-unavailable"
        pods = []
        candidates = [pod for pod in complete_inventory(setup.admin.get(core(namespace, "pods")), 32)
                      if pod["metadata"].get("labels", {}).get("kars.azure.com/sandbox") == name
                      and pod["metadata"].get("namespace") == namespace]
        require(0 < len(candidates) <= 8, UNAVAILABLE)
        for candidate in candidates:
            require(candidate.get("kind") == "Pod" and candidate["spec"].get("serviceAccountName") == "sandbox",
                    UNAVAILABLE)
            ref = owner(candidate, "ReplicaSet")
            require(ref is not None, UNAVAILABLE)
            replica = read(resource(namespace, "replicasets", ref["name"], "/apis/apps/v1"))
            lineage = owner(replica, "Deployment")
            require(identity(replica)[0] == ref["uid"] and lineage is not None
                    and replica["metadata"].get("namespace") == namespace
                    and replica["metadata"].get("name") == ref["name"]
                    and lineage["name"] == name and lineage["uid"] == identity(deployment)[0],
                    UNAVAILABLE)
            pod_name = candidate["metadata"]["name"]
            require(re.fullmatch(r"[a-z0-9][-a-z0-9]{0,252}", pod_name) is not None, UNAVAILABLE)
            pod = read(core(namespace, "pods", pod_name))
            require(identity(pod) == identity(candidate), UNAVAILABLE)
            statuses = [item for item in pod.get("status", {}).get("containerStatuses", [])
                        if item.get("name") == "inference-router"]
            require(len(statuses) == 1, UNAVAILABLE)
            status = statuses[0]
            require(type(status.get("ready")) is bool, UNAVAILABLE)
            restarts = bounded_integer(status.get("restartCount"), 1000000)
            state = container_state(status.get("state", {}))
            entry = {
                "routerReady": status["ready"], "restartCount": restarts,
                "state": state, "lastState": container_state(status.get("lastState", {})),
                "observerVersionMatchesStatus":
                    pod["metadata"].get("annotations", {}).get(VERSION) == observed["version"],
                "observerVersionMatchesTemplate":
                    pod["metadata"].get("annotations", {}).get(VERSION)
                    == template["metadata"].get("annotations", {}).get(VERSION),
                "logs": {"available": False, "category": "no-running-process"},
                "previousLogs": {"available": False, "category": "no-previous-process"},
            }
            if state["phase"] == "running" and isinstance(status.get("containerID"), str) and status["containerID"]:
                entry["logs"] = read_logs(namespace, pod_name)
            if restarts > 0 and entry["lastState"]["phase"] == "terminated":
                entry["previousLogs"] = read_logs(namespace, pod_name, previous=True)
            try:
                events = setup.admin.get(core(namespace, "events") + "?" + urlencode({
                    "fieldSelector": "involvedObject.uid=" + identity(pod)[0]}))
                entry["probeEvents"] = {
                    "available": True, **event_records(events, namespace, pod_name, identity(pod)[0])}
            except READ_ERRORS:
                entry["probeEvents"] = {"available": False, "category": "event-read-unavailable"}
            pods.append(entry)
        require(bool(pods), UNAVAILABLE)
        result["category"] = "snapshot-changed"
        for path, before in anchors.items():
            current = setup.admin.get(path)
            require(identity(current) == identity(before), UNAVAILABLE)
            if before.get("kind") == "Pod":
                require(current.get("status") == before.get("status"), UNAVAILABLE)
        result.update(available=True, category="captured", pods=pods)
    except READ_ERRORS:
        return result
    return result
