#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Disposable Kind acceptance; real MCP workload/protocol, no model execution."""

import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
from mcp_probe import Client

ROOT = Path(__file__).resolve().parents[2]
CONTEXT = "kind-kars-e2e"
WORKSPACE = "kars-system"
OTHER = "e2e-mcp-workspace"
SANDBOX = "e2e-managed-mcp"
SERVER = "e2e-tools"
IMAGE = "kars-mcp-everything:e2e"
PROCESSES = []
DEADLINE = time.monotonic() + 900


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def run(args, body=None, seconds=45):
    seconds = min(seconds, DEADLINE - time.monotonic())
    require(seconds > 0, "Managed MCP acceptance exceeded its total deadline")
    process = subprocess.Popen(args, cwd=ROOT, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, start_new_session=True)
    try:
        output, _ = process.communicate(body, timeout=seconds)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.communicate(timeout=5)
        raise AssertionError(f"Bounded {Path(args[0]).name} operation timed out") from None
    require(process.returncode == 0, f"{Path(args[0]).name} operation failed (exit {process.returncode}); no command output/body logged")
    return output


def kube(*args, body=None, seconds=45):
    return run(["kubectl", "--context", CONTEXT, "--request-timeout=20s", *args], body, seconds)


def get(kind, name, namespace=None):
    value = kube("get", kind, name, *(["-n", namespace] if namespace else []), "--ignore-not-found", "-o", "json")
    return json.loads(value) if value.strip() else None


def create(value):
    return json.loads(kube("create", "-f", "-", "-o", "json", body=json.dumps(value)))


def wait(label, predicate, seconds=180):
    end = min(DEADLINE, time.monotonic() + seconds)
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(1)
    raise AssertionError(f"{label} did not converge")


def delete(kind, name, namespace, uid, wait_for_removal=True):
    obj = get(kind, name, namespace)
    require(obj and obj["metadata"]["uid"] == uid, "Cleanup object incarnation changed")
    resource = {"mcpserver": "mcpservers", "karssandbox": "karssandboxes",
                "inferencepolicy": "inferencepolicies"}.get(kind)
    path = (f"/apis/kars.azure.com/v1alpha1/namespaces/{namespace}/{resource}/{name}"
            if resource else f"/api/v1/namespaces/{name}")
    kube("delete", "--raw", path, "-f", "-", body=json.dumps({
        "apiVersion": "v1", "kind": "DeleteOptions",
        "preconditions": {"uid": uid, "resourceVersion": obj["metadata"]["resourceVersion"]},
    }))
    if wait_for_removal:
        wait(f"{kind} cleanup", lambda: get(kind, name, namespace) is None)


def ready(namespace):
    obj = get("mcpserver", SERVER, namespace)
    status = obj.get("status", {}) if obj else {}
    return obj if (status.get("phase") == "Ready"
        and status.get("observedGeneration") == obj["metadata"]["generation"]
        and status.get("workloadImage") == IMAGE
        and "echo" in status.get("discoveredTools", [])) else False


def build_image():
    run(["docker", "build", "-t", IMAGE, "-f", "sandbox-images/mcp-everything/Dockerfile",
         "sandbox-images/mcp-everything"], seconds=360)
    run(["kind", "load", "docker-image", IMAGE, "--name", "kars-e2e"], seconds=120)
    print("MCP-PASS Built/loaded the actual locked Everything server image; no image published", flush=True)


def test():
    require(get("namespace", WORKSPACE), "Expected disposable Kind controller namespace is absent")
    other = create({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": OTHER}})
    sources = []
    for namespace in (WORKSPACE, OTHER):
        sources.append(create({"apiVersion": "kars.azure.com/v1alpha1", "kind": "McpServer",
            "metadata": {"name": SERVER, "namespace": namespace},
            "spec": {"managed": {"preset": "everything"}, "allowedTools": ["echo"],
                     "allowedSandboxes": {"matchLabels": {"e2e-mcp": "allowed"}}}}))
    first = wait("first managed MCP protocol readiness", lambda: ready(WORKSPACE))
    second = wait("second managed MCP protocol readiness", lambda: ready(OTHER))
    require(first["status"]["workloadRef"] != second["status"]["workloadRef"],
            "Same-named servers across workspaces shared a workload")
    references = []
    for source in (first, second):
        namespace, name = source["status"]["workloadRef"].split("/")
        ns = get("namespace", namespace)
        deployment = get("deployment", name, namespace)
        service = get("service", name, namespace)
        policy = get("networkpolicy", name, namespace)
        uid = source["metadata"]["uid"]
        selector = {"kars.azure.com/mcp-source-uid": uid}
        require(ns["metadata"]["uid"] == source["status"]["managedNamespaceUid"], "Namespace UID proof differs")
        require(deployment["spec"]["selector"]["matchLabels"] == selector and service["spec"]["selector"] == selector,
                "Managed Deployment/Service selectors are not source-UID bound")
        require(policy["spec"]["podSelector"]["matchLabels"] == selector, "Managed NetworkPolicy selector differs")
        require(deployment["spec"]["template"]["spec"].get("automountServiceAccountToken") is False,
                "Managed workload acquired a Kubernetes token")
        require(source["status"]["workloadGeneration"] == deployment["metadata"]["generation"], "Probe covers a stale Deployment generation")
        references.append((namespace, name, deployment["metadata"]["uid"]))
    print("MCP-PASS Two real same-named MCP servers qualify independently with UID-scoped selectors and protocol probes", flush=True)

    inference = create({"apiVersion": "kars.azure.com/v1alpha1", "kind": "InferencePolicy",
        "metadata": {"name": SANDBOX, "namespace": WORKSPACE},
        "spec": {"appliesTo": {"sandboxName": SANDBOX},
                 "modelPreference": {"primary": {"provider": "azure-openai", "deployment": "gpt-4.1"}}}})
    sandbox = create({"apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSandbox",
        "metadata": {"name": SANDBOX, "namespace": WORKSPACE, "labels": {"e2e-mcp": "allowed"}},
        "spec": {"runtime": {"kind": "BYO", "byo": {"image": "kars-sandbox-e2e:dev",
            "contractVersion": "v1", "command": ["/bin/sh"], "args": ["-c", "sleep infinity"]}},
            "sandbox": {"isolation": "standard"}, "inferenceRef": {"name": SANDBOX},
            "governance": {"mcpServerRefs": [{"name": SERVER}]}}})
    runtime_namespace = f"kars-{SANDBOX}"
    wait("MCP Sandbox Deployment", lambda: get("deployment", SANDBOX, runtime_namespace))
    kube("rollout", "status", f"deployment/{SANDBOX}", "-n", runtime_namespace, "--timeout=180s", seconds=190)
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    process = subprocess.Popen(["kubectl", "--context", CONTEXT, "--request-timeout=20s",
        "-n", runtime_namespace, "port-forward", f"deployment/{SANDBOX}", f"{port}:8443", "--address=127.0.0.1"],
        cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    PROCESSES.append(process)
    try:
        rpc = Client(port, SERVER)
        def catalog():
            require(process.poll() is None, "MCP port-forward exited")
            try:
                code, body = rpc.call("tools/list", {})
                return body if code == 200 and any(tool.get("name") == "e2e_tools.echo"
                    for tool in (body or {}).get("result", {}).get("tools", [])) else False
            except (OSError, ValueError, AssertionError):
                return False
        wait("actual routed MCP catalog after reconciliation", catalog, seconds=120)
        code, body = rpc.call("tools/call", {"name": "e2e_tools.echo", "arguments": {"message": "mcp-kind-proof"}})
        require(code == 200 and "error" not in body and not body["result"].get("isError")
                and "mcp-kind-proof" in json.dumps(body["result"]), "Real MCP echo did not execute through the router")
        code, _ = rpc.call("tools/list", {}, "unmounted")
        require(code == 404, "Unknown MCP server scope was not rejected")
        print("MCP-PASS Real router-scoped discovery and Everything echo work from an alive BYO fixture; no model run claimed", flush=True)
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        PROCESSES.remove(process)

    delete("karssandbox", SANDBOX, WORKSPACE, sandbox["metadata"]["uid"])
    wait("MCP Sandbox namespace cleanup", lambda: get("namespace", runtime_namespace) is None)
    delete("inferencepolicy", SANDBOX, WORKSPACE, inference["metadata"]["uid"])
    delete("mcpserver", SERVER, WORKSPACE, sources[0]["metadata"]["uid"])
    namespace, name, _ = references[0]
    for kind in ("deployment", "service", "networkpolicy"):
        require(get(kind, name, namespace) is None, "Owned managed MCP resource survived finalization")
    other_namespace, other_name, other_uid = references[1]
    require(get("deployment", other_name, other_namespace)["metadata"]["uid"] == other_uid,
            "Deleting one source changed the same-named server in another workspace")
    delete("mcpserver", SERVER, OTHER, sources[1]["metadata"]["uid"])
    delete("namespace", OTHER, None, other["metadata"]["uid"])
    require(get("namespace", namespace), "Shared managed namespace was deleted")
    print("MCP-PASS Exact owned cleanup preserves the other workspace and shared managed namespace", flush=True)

    # Simulate a namespace whose ownership has been explicitly withdrawn.
    # No server resources remain; the sentinel belongs solely to this fixture.
    ns = get("namespace", namespace)
    kube("patch", "namespace", namespace, "--type=merge", "-p", json.dumps({"metadata": {
        "uid": ns["metadata"]["uid"], "resourceVersion": ns["metadata"]["resourceVersion"],
        "annotations": {"kars.azure.com/mcp-namespace-claim": None},
    }}))
    create({"apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": "mcp-foreign-sentinel", "namespace": namespace}, "data": {"value": "preserve"}})
    blocked = create({"apiVersion": "kars.azure.com/v1alpha1", "kind": "McpServer",
        "metadata": {"name": SERVER, "namespace": WORKSPACE},
        "spec": {"managed": {"preset": "everything"}, "allowedTools": ["echo"]}})
    def blocked_on_owner():
        current = get("mcpserver", SERVER, WORKSPACE)
        status = current.get("status", {})
        return (status.get("phase") == "Degraded"
            and status.get("observedGeneration") == current["metadata"]["generation"]
            and any("unowned" in condition.get("message", "") for condition in status.get("conditions", [])))
    wait("actual unowned namespace refusal", blocked_on_owner)
    current = get("namespace", namespace)
    require(current["metadata"]["uid"] == ns["metadata"]["uid"]
            and "kars.azure.com/mcp-namespace-claim" not in current["metadata"].get("annotations", {}),
            "Controller adopted or replaced the unowned namespace")
    require(get("configmap", "mcp-foreign-sentinel", namespace)["data"]["value"] == "preserve",
            "Unowned namespace contents were modified")
    require(json.loads(kube("get", "deployments", "-n", namespace, "-o", "json"))["items"] == [],
            "A workload was launched into an unowned namespace")
    delete("mcpserver", SERVER, WORKSPACE, blocked["metadata"]["uid"], wait_for_removal=False)
    delete("namespace", namespace, None, ns["metadata"]["uid"])
    wait("blocked MCP finalizer after explicit fixture removal", lambda: get("mcpserver", SERVER, WORKSPACE) is None)
    print("MCP-PASS Actual API namespace-ownership refusal preserves the occupant and sentinel; explicit UID/RV fixture cleanup only", flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("phase", choices=("prepare", "test"))
    args = parser.parse_args()
    try:
        build_image() if args.phase == "prepare" else test()
        return 0
    except Exception as error:
        print(f"MCP-FAIL {args.phase}: {str(error) if isinstance(error, AssertionError) else type(error).__name__}", flush=True)
        for namespace in (WORKSPACE, OTHER):
            try:
                obj = get("mcpserver", SERVER, namespace)
                if obj:
                    print("MCP-DIAG", json.dumps({"namespace": namespace, "uid": obj["metadata"].get("uid"),
                        "phase": obj.get("status", {}).get("phase"), "generation": obj["metadata"].get("generation"),
                        "observedGeneration": obj.get("status", {}).get("observedGeneration")}), flush=True)
            except Exception:
                pass
        return 1
    finally:
        for process in PROCESSES:
            process.terminate()


if __name__ == "__main__":
    raise SystemExit(main())
