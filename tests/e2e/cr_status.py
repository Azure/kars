# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Read one complete current-generation CR status within the caller's deadline."""

import argparse
import json
import math
import re
import subprocess
import sys
import time


KINDS = ("inferencepolicy", "karsmemory", "karseval", "egressapproval")


class StatusObservationError(RuntimeError):
    pass


def project(value, require_hosts=False):
    if not isinstance(value, dict) or not isinstance(value.get("metadata"), dict):
        raise StatusObservationError("invalid resource metadata")
    metadata = value["metadata"]
    uid, generation, version = (metadata.get(key) for key in ("uid", "generation", "resourceVersion"))
    if not isinstance(uid, str) or not uid or not isinstance(version, str) or not version:
        raise StatusObservationError("resource identity or version is missing")
    if type(generation) is not int or generation < 1:
        raise StatusObservationError("invalid resource generation")
    identity = uid, generation
    status = value.get("status")
    if status is None:
        return identity, None
    if not isinstance(status, dict):
        raise StatusObservationError("invalid status object")
    conditions = status.get("conditions")
    if conditions is None:
        return identity, None
    if not isinstance(conditions, list) or any(not isinstance(item, dict) for item in conditions):
        raise StatusObservationError("invalid status conditions")
    ready = [item for item in conditions if item.get("type") == "Ready"]
    if len(ready) > 1:
        raise StatusObservationError("duplicate Ready conditions")
    if not ready:
        return identity, None
    for observed in (status.get("observedGeneration"), ready[0].get("observedGeneration")):
        if observed is None:
            return identity, None
        if type(observed) is not int or observed < 0:
            raise StatusObservationError("invalid observed generation")
        if observed != generation:
            return identity, None
    fields = (status.get("phase"), ready[0].get("status"), ready[0].get("reason"))
    if any(field is None or field == "" for field in fields):
        return identity, None
    if any(not isinstance(field, str) or len(field) > 1024
           or any(character in field for character in "|\r\n") for field in fields):
        raise StatusObservationError("invalid status projection")
    if require_hosts:
        hosts = status.get("hostCount")
        if hosts is None:
            return identity, None
        if type(hosts) is not int or hosts < 0:
            raise StatusObservationError("invalid host count")
        fields += (str(hosts),)
    return identity, fields


def observe(kind, name, namespace, deadline):
    if kind not in KINDS or not re.fullmatch(r"[a-z0-9](?:[-a-z0-9.]*[a-z0-9])?", name):
        raise StatusObservationError("invalid fixture resource")
    if len(name) > 253 or len(namespace) > 63 or not re.fullmatch(
        r"[a-z0-9](?:[-a-z0-9]*[a-z0-9])?", namespace
    ):
        raise StatusObservationError("invalid fixture namespace or name")
    remaining = deadline - time.time()
    if not math.isfinite(remaining) or remaining > 45:
        raise StatusObservationError("invalid status deadline")
    end = time.monotonic() + remaining
    identity = None
    while (remaining := end - time.monotonic()) > 0:
        try:
            result = subprocess.run(
                ["kubectl", "--context", "kind-kars-e2e", f"--request-timeout={remaining:.3f}s",
                 "get", kind, name, "-n", namespace, "-o", "json"],
                capture_output=True, text=True, timeout=remaining,
            )
        except subprocess.TimeoutExpired:
            raise StatusObservationError("status read exceeded its deadline") from None
        except OSError:
            raise StatusObservationError("status reader could not start") from None
        if result.returncode != 0:
            raise StatusObservationError("status read failed")
        if len(result.stdout) > 1_048_576:
            raise StatusObservationError("status response exceeded its bound")
        try:
            value = json.loads(result.stdout)
        except json.JSONDecodeError:
            raise StatusObservationError("invalid status JSON") from None
        current, fields = project(value, require_hosts=kind == "egressapproval")
        if identity is not None and current != identity:
            raise StatusObservationError("resource identity or intent changed")
        identity = current
        remaining = end - time.monotonic()
        if remaining <= 0:
            break
        # The existing unscheduled evaluator assertion waits specifically for Pending/False.
        if fields is not None and (kind != "karseval" or fields[:2] == ("Pending", "False")):
            return fields
        time.sleep(min(1, remaining))
    raise StatusObservationError("current status was not observed before the deadline")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kind", choices=KINDS, required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("--namespace", required=True)
    parser.add_argument("--deadline", type=float, required=True)
    args = parser.parse_args()
    try:
        fields = observe(args.kind, args.name, args.namespace, args.deadline)
    except StatusObservationError as error:
        print(f"CR-STATUS-FAILURE: {error}", file=sys.stderr)
        return 1
    print("|".join(fields))
    return 0


if __name__ == "__main__":
    sys.exit(main())
