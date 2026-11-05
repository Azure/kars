# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Failure-only controller process facts; never publish log bodies or error values."""

import json
import re

from native_api import ROOT, command, core, require
from observation_diagnostics import READ_ERRORS, recheck_actor, resolve_actor
from rollout_diagnostics import bounded_integer, container_state

UNAVAILABLE = "Controller process diagnostic unavailable"
LEADER_ERRORS = {
    "renewal deadline exceeded": "renewal-deadline",
    "lease GET deadline exceeded": "read-deadline",
    "lease GET failed while holder": "holder-read-failed",
    "controller lease expired, disappeared, changed owner or was replaced": "lease-changed",
    "lease write deadline exceeded": "write-deadline",
    "lease renewal failed": "renewal-failed",
    "lease write completed too late": "late-write",
}


def project(raw, sources):
    require(isinstance(raw, str) and len(raw.encode()) <= 131072, UNAVAILABLE)
    result = {"coverage": "bounded-process-log-tail", "errorRecords": 0,
              "panicLocations": [], "leaderFailures": []}
    for line in raw.splitlines()[-512:]:
        # Only a checked-in source coordinate is retained, never the thread or panic message.
        panic = re.fullmatch(r"thread '.+' panicked at (controller/src/[a-zA-Z0-9_/.-]+\.rs):([1-9][0-9]{0,5}):[1-9][0-9]{0,4}:", line)
        if panic and panic[1] in sources:
            result["panicLocations"].append({"source": panic[1], "line": int(panic[2])})
        leader = LEADER_ERRORS.get(line.removeprefix("Error: leader-election: ")) if line.startswith("Error: leader-election: ") else None
        if leader:
            result["leaderFailures"].append(leader)
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if isinstance(value, dict) and value.get("level") == "ERROR":
            target = value.get("target")
            if isinstance(target, str) and (target == "kars_controller" or target.startswith("kars_controller::")):
                result["errorRecords"] += 1
    result["panicLocations"] = result["panicLocations"][-8:]
    result["leaderFailures"] = result["leaderFailures"][-8:]
    return result


def read_logs(actor, pod, previous, sources):
    try:
        raw = command("kubectl", "logs", "-n", actor["namespace"], pod["name"], "-c", actor["container"],
                      "--tail=512", "--limit-bytes=131072", "--request-timeout=10s",
                      *(["--previous"] if previous else []), timeout=15)
        return {"available": True, **project(raw, sources)}
    except READ_ERRORS:
        return {"available": False, "category": "log-read-unavailable"}


def collect(setup):
    try:
        actor = resolve_actor(setup, "controller", None)
        source = ROOT / ".native/core"
        sources = {path.relative_to(source).as_posix() for path in (source / "controller/src").rglob("*.rs")}
        pods = []
        for pod in actor["pods"]:
            observed = actor["anchors"][core(actor["namespace"], "pods", pod["name"])]
            states = [item for item in observed.get("status", {}).get("containerStatuses", [])
                      if item.get("name") == actor["container"]]
            require(len(states) == 1, UNAVAILABLE)
            state = states[0]
            current = container_state(state.get("state", {}))
            restarts = bounded_integer(state.get("restartCount"), 1000000)
            pods.append({**pod, "restartCount": restarts, "state": current,
                         "lastState": container_state(state.get("lastState", {})),
                         "logs": read_logs(actor, pod, False, sources) if current["phase"] == "running"
                         else {"available": False, "category": "process-not-running"},
                         "previousLogs": read_logs(actor, pod, True, sources) if restarts
                         else {"available": False, "category": "no-previous-process"}})
        recheck_actor(setup, actor)
        return {"available": True, "diagnosticOnly": True, "pods": pods}
    except READ_ERRORS:
        return {"available": False, "diagnosticOnly": True, "category": "identity-or-state-unavailable"}
