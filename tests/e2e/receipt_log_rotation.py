# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real controller append, immutable-prefix and packaged CLI proof in CI Kind."""

import argparse
import copy
import json
import os
from pathlib import Path
import re
import subprocess
import time
import uuid

from sre_authority import registration_schema as api

NS = "kars-system"
CMS = f"/api/v1/namespaces/{NS}/configmaps"
TASKS = f"/apis/kars.azure.com/v1alpha1/namespaces/{NS}/karstasks"
HEAD = "kars-receipt-log"
OVERFLOW = f"{HEAD}-000001"
THRESHOLD = 700 * 1024


class ProbeFailure(RuntimeError):
    """Fixed stage only; never resource bodies, prompts or API messages."""


def require(condition, stage):
    if not condition:
        raise ProbeFailure(stage)


def identity(obj, kind, name):
    require(isinstance(obj, dict) and obj.get("kind") == kind, "resource-kind")
    meta = obj.get("metadata", {})
    require(meta.get("name") == name and meta.get("namespace") == NS
            and all(isinstance(meta.get(k), str) and meta[k] for k in ("uid", "resourceVersion"))
            and meta.get("deletionTimestamp") is None, "resource-identity")
    return meta


def read(port, path, kind, name):
    code, obj = api.request(port, "GET", path)
    require(code == 200, "resource-read")
    identity(obj, kind, name)
    return obj


def conflict(code, body, name):
    return (code == 409 and isinstance(body, dict) and body.get("kind") == "Status"
            and body.get("status") == "Failure" and body.get("code") == 409
            and body.get("reason") == "Conflict"
            and body.get("details", {}).get("name") == name)


def checkpoint_lag(report):
    checks = report.get("checks")
    if report.get("ok") is not False or not isinstance(checks, list) or not checks:
        return False
    if any(not isinstance(c, dict) or type(c.get("ok")) is not bool for c in checks):
        return False
    failures = [c for c in checks if not c["ok"]]
    if len(failures) != 1 or failures[0].get("name") != "checkpoint":
        return False
    match = re.fullmatch(
        r"checkpoint \(size ([0-9]+)\) diverges from the live log \(size ([0-9]+)\)"
        r" \u2014 history may have been rewritten", failures[0].get("detail", ""))
    return (match is not None and int(match[1]) < int(match[2])
            and any(c.get("name") == "inclusion" and c["ok"] for c in checks))


def cli(root, *args, allow_checkpoint_lag=False):
    env = {**os.environ, "KARS_NAMESPACE": NS, "POD_NAMESPACE": NS}
    try:
        result = subprocess.run(
            ["node", str(Path(root) / "cli/dist/index.js"), "receipt", *args, "--format", "json"],
            cwd=root, env=env, capture_output=True, text=True, timeout=30, check=False)
    except (OSError, subprocess.TimeoutExpired):
        raise ProbeFailure("packaged-cli-unavailable") from None
    require(result.returncode in ((0, 2) if allow_checkpoint_lag else (0,)), "packaged-cli-failed")
    try:
        report = json.loads(result.stdout)
    except ValueError:
        raise ProbeFailure("packaged-cli-json") from None
    require(isinstance(report, dict), "packaged-cli-report")
    if result.returncode == 2:
        require(checkpoint_lag(report), "packaged-cli-verification-failed")
        return None
    return report


def preserved(current, original):
    require(current["metadata"]["uid"] == original["metadata"]["uid"], "head-uid-changed")
    require(current.get("immutable") is not True, "head-already-sealed")
    require(not current["metadata"].get("ownerReferences"), "head-foreign-owner")
    data = current.get("data", {})
    raw = data.get("chain.json")
    require(isinstance(raw, str) and len(raw.encode()) <= THRESHOLD, "head-size")
    try:
        entries = json.loads(raw)
        old_entries = json.loads(original["data"]["chain.json"])
    except (ValueError, KeyError, TypeError):
        raise ProbeFailure("head-json") from None
    require(isinstance(entries, list) and entries and entries[:len(old_entries)] == old_entries,
            "head-prefix-changed")
    return raw, entries


def pad_head(root, port, original):
    for _ in range(3):
        current = read(port, f"{CMS}/{HEAD}", "ConfigMap", HEAD)
        raw, entries = preserved(current, original)
        observed = cli(root, "log")
        require(observed.get("intact") is True and observed.get("entries") == entries,
                "initial-cli-chain")
        replacement = copy.deepcopy(current)
        replacement["data"]["chain.json"] = raw + " " * (THRESHOLD - len(raw.encode()))
        require(json.loads(replacement["data"]["chain.json"]) == entries, "padding-changes-chain")
        code, saved = api.request(port, "PUT", f"{CMS}/{HEAD}", replacement)
        if conflict(code, saved, HEAD):
            continue
        require(code == 200, "head-padding-write")
        identity(saved, "ConfigMap", HEAD)
        require(saved["metadata"]["uid"] == current["metadata"]["uid"]
                and saved.get("data") == replacement["data"], "head-padding-ack")
        return saved, entries
    raise ProbeFailure("head-padding-contention")


def same_task(current, original):
    identity(current, "KarsTask", original["metadata"]["name"])
    require(current["metadata"]["uid"] == original["metadata"]["uid"]
            and current["metadata"].get("generation") == original["metadata"].get("generation")
            and current.get("spec") == original.get("spec"), "task-authority-changed")


def cleanup_task(port, original):
    name = original["metadata"]["name"]
    for _ in range(3):
        current = read(port, f"{TASKS}/{name}", "KarsTask", name)
        same_task(current, original)
        code, body = api.request(port, "DELETE", f"{TASKS}/{name}", {
            "apiVersion": "v1", "kind": "DeleteOptions",
            "preconditions": {k: current["metadata"][k] for k in ("uid", "resourceVersion")},
        })
        if conflict(code, body, name):
            continue
        require(code in (200, 202), "task-cleanup-delete")
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            code, body = api.request(port, "GET", f"{TASKS}/{name}")
            if code == 404:
                require(isinstance(body, dict) and body.get("kind") == "Status"
                        and body.get("reason") == "NotFound"
                        and body.get("details", {}).get("name") == name, "task-cleanup-notfound")
                return
            require(code == 200 and body.get("metadata", {}).get("uid")
                    == original["metadata"]["uid"], "task-cleanup-replaced")
            time.sleep(0.2)
        raise ProbeFailure("task-cleanup-deadline")
    raise ProbeFailure("task-cleanup-contention")


def immutable_denial(code, body):
    if not isinstance(body, dict):
        return False
    details = body.get("details", {}) if isinstance(body, dict) else {}
    return (code == 422 and body.get("kind") == "Status" and body.get("status") == "Failure"
            and body.get("code") == 422 and body.get("reason") == "Invalid"
            and details.get("name") == HEAD and details.get("kind") in ("ConfigMap", "configmaps")
            and any(c.get("field") == "data" and c.get("reason") == "FieldValueForbidden"
                    and "immutable" in c.get("message", "")
                    for c in details.get("causes", []) if isinstance(c, dict)))


def await_verified_receipt(root, port, task, prefix, deadline):
    checkpoint_uid = None
    name = task["metadata"]["name"]
    while time.monotonic() < deadline:
        same_task(read(port, f"{TASKS}/{name}", "KarsTask", name), task)
        log = cli(root, "log")
        require(log.get("intact") is True and log.get("entries", [])[:len(prefix)] == prefix,
                "complete-cli-chain")
        cp_name = "kars-receipt-checkpoint"
        code, checkpoint = api.request(port, "GET", f"{CMS}/{cp_name}")
        if code == 404:
            require(isinstance(checkpoint, dict) and checkpoint.get("kind") == "Status"
                    and checkpoint.get("reason") == "NotFound"
                    and checkpoint.get("details", {}).get("name") == cp_name, "checkpoint-notfound")
            time.sleep(0.2)
            continue
        require(code == 200, "checkpoint-read")
        meta = identity(checkpoint, "ConfigMap", cp_name)
        require(checkpoint_uid is None or meta["uid"] == checkpoint_uid, "checkpoint-replaced")
        checkpoint_uid = meta["uid"]
        data = checkpoint.get("data", {})
        require(isinstance(data, dict) and all(
            isinstance(data.get(key), str) and data[key]
            for key in ("treeSize", "rootHash", "keyId", "signature", "note")),
            "checkpoint-shape")
        verified = cli(root, "verify", name, "-n", NS, allow_checkpoint_lag=True)
        if verified is None:
            time.sleep(0.2)
            continue
        require(verified.get("ok") is True and all(any(
            c.get("name") == required and c.get("ok") is True
            for c in verified.get("checks", [])) for required in ("inclusion", "checkpoint")),
            "new-receipt-inclusion-checkpoint")
        return log
    raise ProbeFailure("receipt-checkpoint-convergence-deadline")


def run(root, port):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        code, original = api.request(port, "GET", f"{CMS}/{HEAD}")
        if code == 200:
            identity(original, "ConfigMap", HEAD)
            break
        require(code == 404 and isinstance(original, dict)
                and original.get("kind") == "Status" and original.get("reason") == "NotFound",
                "initial-head-read")
        time.sleep(0.2)
    else:
        raise ProbeFailure("initial-head-deadline")
    padded, prefix = pad_head(root, port, original)
    name = f"e2e-receipt-rotation-{uuid.uuid4().hex[:12]}"
    request = {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTask",
        "metadata": {"name": name, "namespace": NS},
        "spec": {"objective": "Verify receipt log rotation without launching an agent",
                 "envelope": {"tier": 1, "authorityCeiling": 1, "delegationDepth": 0}},
    }
    code, task = api.request(port, "POST", TASKS, request)
    require(code == 201, "task-create")
    identity(task, "KarsTask", name)
    require(task["spec"] == request["spec"] and task["metadata"].get("generation") == 1,
            "task-create-ack")
    try:
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            current = read(port, f"{TASKS}/{name}", "KarsTask", name)
            same_task(current, task)
            code, segment = api.request(port, "GET", f"{CMS}/{OVERFLOW}")
            if code == 200:
                identity(segment, "ConfigMap", OVERFLOW)
                try:
                    segment_entries = json.loads(segment.get("data", {}).get("chain.json", ""))
                except ValueError:
                    raise ProbeFailure("overflow-json") from None
                if any(e.get("receipt") == f"{NS}/{name}" for e in segment_entries):
                    break
            else:
                require(code == 404 and isinstance(segment, dict)
                        and segment.get("reason") == "NotFound", "overflow-read")
            time.sleep(0.5)
        else:
            raise ProbeFailure("controller-rotation-deadline")
        sealed = read(port, f"{CMS}/{HEAD}", "ConfigMap", HEAD)
        require(sealed["metadata"]["uid"] == padded["metadata"]["uid"]
                and sealed.get("data") == padded.get("data") and sealed.get("immutable") is True,
                "sealed-prefix")
        data = segment.get("data", {})
        require(data.get("segmentIndex") == "1"
                and data.get("previousRootHash") == prefix[-1]["entryHash"], "overflow-binding")
        # Append and checkpoint publication are separate controller writes.
        # Only authenticated checkpoint/log convergence may consume the remainder
        # of the original deadline; no signature/authority failure is retried.
        log = await_verified_receipt(root, port, task, prefix, deadline)
        attempt = copy.deepcopy(sealed)
        attempt["data"]["chain.json"] += " "
        code, body = api.request(port, "PUT", f"{CMS}/{HEAD}?dryRun=All", attempt)
        require(immutable_denial(code, body), "legacy-writer-not-immutable-denial")
        return {"prefixEntries": len(prefix), "totalEntries": len(log["entries"]),
                "sealedBytes": THRESHOLD, "overflow": OVERFLOW,
                "controllerAppend": True, "cliInclusion": True, "immutableDenial": 422}
    finally:
        cleanup_task(port, task)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    args = parser.parse_args()
    root = Path(args.root).resolve()
    require(os.environ.get("GITHUB_ACTIONS") == "true", "ci-only-fixture")
    require(Path(os.environ.get("KUBECONFIG", "")).resolve() == root / ".e2e-kind-kubeconfig",
            "owned-kind-kubeconfig")
    require(api.command("receipt-context", ["kubectl", "config", "current-context"],
                        root=root).strip() == api.CONTEXT, "current-kind-context")
    with api.kind_proxy(root) as (port, _):
        report = run(root, port)
    print("RECEIPT-ROTATION " + json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
