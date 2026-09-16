# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded metadata-only audit snapshots, including retained rotated files."""

import json
import re

from native_api import BRIDGE, WRITER, CommandFailure, Failure, command, require, resource, until

DIRECTORY = "/var/log/kars-native-audit"
CONTROL_PLANE = "bridge-native-control-plane"
MAX_BYTES = 80 * 1024 * 1024
AUDIT_ID = re.compile(r"[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}")
NAME = re.compile(r"audit(?:-[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}-[0-9]{2}-[0-9]{2}\.[0-9]{3})?\.log")


class AuditSnapshot(list):
    def __init__(self, events, marker):
        super().__init__(events)
        self.marker = marker


class UnsettledInventory(Failure):
    """The active log is between its rotation rename and recreation."""


def paths():
    names = command("docker", "exec", CONTROL_PLANE, "find", DIRECTORY,
                    "-maxdepth", "1", "-type", "f", "-name", "audit*.log",
                    timeout=30).splitlines()
    require(len(names) <= 3 and len(set(names)) == len(names)
            and all(name.startswith(DIRECTORY + "/") and
                    NAME.fullmatch(name[len(DIRECTORY) + 1:]) for name in names),
            "Native audit file inventory is unavailable or outside its bounded scope")
    if DIRECTORY + "/audit.log" not in names:
        raise UnsettledInventory("Native audit rotation has not published its active file")
    return sorted(names)


def parse_events(raw, namespace=None):
    require(len(raw.encode()) <= MAX_BYTES, "Native audit snapshot exceeds its bounded size")
    unique = {}
    for line in raw.splitlines():
        if not line:
            continue
        event = json.loads(line)
        require(isinstance(event, dict), "Native audit contains an invalid record")
        if event.get("stage") != "ResponseComplete" or event.get("user", {}).get("username") != (
                f"system:serviceaccount:{BRIDGE}:{WRITER}"):
            continue
        if namespace is not None and event.get("objectRef", {}).get("namespace") != namespace:
            continue
        audit_id = event.get("auditID")
        require(isinstance(audit_id, str) and AUDIT_ID.fullmatch(audit_id),
                "Native audit record omitted a valid request identity")
        if audit_id in unique:
            require(unique[audit_id] == event, "Native audit contains conflicting records for one request")
        unique[audit_id] = event
    return list(unique.values())


def read(namespace=None):
    for _ in range(3):
        try:
            before = paths()
            raw = command("docker", "exec", CONTROL_PLANE, "cat", *before, timeout=30)
            if before != paths():
                continue
            return parse_events(raw, namespace)
        except (CommandFailure, json.JSONDecodeError, UnsettledInventory):
            # Rotation can retire a file between inventory and open.
            continue
    raise Failure("Native audit snapshot did not settle; no complete event proof is available")


def barrier(setup, actor, namespace):
    marker = actor.audit_marker(resource(namespace, "karscredentialgrants", "workspace"))

    def observed():
        events = setup.audit(namespace)
        matches = [event for event in events if event["auditID"] == marker]
        if not matches:
            return None
        require(len(matches) == 1 and matches[0].get("verb") == "get"
                and matches[0].get("objectRef", {}).get("resource") == "karscredentialgrants"
                and matches[0]["objectRef"].get("name") == "workspace"
                and matches[0].get("responseStatus", {}).get("code") == 200,
                "Native audit barrier does not match its actual successful request")
        return AuditSnapshot(events, marker)

    return until("actual metadata audit barrier", observed, 30)


def changes(before, after):
    require(isinstance(before, AuditSnapshot) and isinstance(after, AuditSnapshot)
            and before.marker != after.marker, "Native audit interval needs two distinct actual barriers")
    ids = {event["auditID"] for event in before}
    require(before.marker in ids and any(event["auditID"] == before.marker for event in after),
            "Native audit interval lost its starting barrier; event coverage is incomplete")
    return [event for event in after if event["auditID"] not in ids]
