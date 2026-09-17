# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Configure only Bridge's Helm-owned AKS composer ingress, never its workloads."""

import argparse
import base64
import copy
import json
from pathlib import Path
import subprocess
import tempfile
import time

import yaml

CHART = Path(__file__).resolve().parent / "helm/kars-bridge"
RUNTIME = "kars-bridge-orchestrator"
SANDBOX = "bridge-orchestrator"


def documents(manifest):
    objects = {}
    for obj in yaml.safe_load_all(manifest):
        if not obj:
            continue
        metadata = obj["metadata"]
        key = (obj["apiVersion"], obj["kind"], metadata.get("namespace", ""), metadata["name"])
        if key in objects:
            raise ValueError("Helm manifest contains duplicate resource identities")
        objects[key] = obj
    return objects


def require_policy_only(before, after, release):
    key = ("networking.k8s.io/v1", "NetworkPolicy", RUNTIME, release + "-orchestrator-proxy")
    old, new = documents(before), documents(after)
    old.pop(key, None)
    new.pop(key, None)
    if old != new:
        raise ValueError(
            "Refusing non-network changes: this command cannot upgrade images, templates, "
            "RBAC, credentials or namespaces. Use the matching installed chart or a "
            "separately qualified upgrade; private-consumer template migration is not bypassed."
        )


def live_key(obj):
    metadata = obj["metadata"]
    return (obj["apiVersion"], obj["kind"], metadata.get("namespace", ""), metadata["name"])


def matches_declared(expected, live):
    if expected is None or expected == {} or expected == []:
        return live is None or expected == live
    if isinstance(expected, dict):
        return isinstance(live, dict) and all(
            matches_declared(value, live.get(key)) for key, value in expected.items()
        )
    if isinstance(expected, list):
        return isinstance(live, list) and len(expected) == len(live) and all(
            matches_declared(left, right) for left, right in zip(expected, live)
        )
    return type(expected) is type(live) and expected == live


def require_live_compatible(before, live, release):
    expected = documents(before)
    expected.pop(("networking.k8s.io/v1", "NetworkPolicy", RUNTIME, release + "-orchestrator-proxy"), None)
    if expected.keys() != live.keys():
        raise ValueError("Non-policy release resources are missing or replaced; refusing reconciliation")
    for key, original in expected.items():
        declared = copy.deepcopy(original)
        if declared["kind"] == "Secret":
            for name, value in declared.pop("stringData", {}).items():
                declared.setdefault("data", {})[name] = base64.b64encode(value.encode()).decode()
        current = live[key]
        metadata = current.get("metadata", {})
        if (not metadata.get("uid") or not metadata.get("resourceVersion")
                or metadata.get("deletionTimestamp") or not matches_declared(declared, current)):
            raise ValueError(f"Live {key[1]}/{key[3]} differs from the declared release; no upgrade attempted")


def require_live_unchanged(before, after):
    def configuration(objects):
        result = copy.deepcopy(objects)
        for obj in result.values():
            obj.pop("status", None)
            for field in ["resourceVersion", "managedFields"]:
                obj["metadata"].pop(field, None)
        return result
    if configuration(before) != configuration(after):
        raise RuntimeError(
            "Non-policy live resources changed during the operation; qualification is not established. "
            "Preserve the current state for operator review; no automatic rollback was attempted."
        )


class Configuration:
    def __init__(self, args):
        self.args = args
        self.scope = ["--kubeconfig", args.kubeconfig]
        self.helm = ["helm", *self.scope, "--kube-context", args.context]
        self.kubectl = ["kubectl", *self.scope, "--context", args.context, "--request-timeout=15s"]
        self.release = ["--namespace", args.namespace]

    def command(self, command, stage, timeout=90):
        result = subprocess.run(command, text=True, capture_output=True, timeout=timeout)
        if result.returncode:
            # Helm output can contain values and rendered Secrets.
            raise RuntimeError(f"{stage} failed (exit {result.returncode}); no success or rollback is assumed")
        return result.stdout

    def get(self, kind, name, namespace=None, optional=False):
        command = [*self.kubectl, "get", kind, name, "-o", "json"]
        if namespace:
            command += ["--namespace", namespace]
        if optional:
            command += ["--ignore-not-found"]
        raw = self.command(command, f"Read {kind}/{name}")
        return json.loads(raw) if raw.strip() else None

    def helm_read(self, operation, output_json=False):
        args = [*self.helm, "get", operation, self.args.release, *self.release]
        if output_json:
            # Computed (--all) values lose null deletions. Reusing them as an
            # overlay can restore chart defaults, including removed OIDC roles.
            args += ["--output", "json"]
        raw = self.command(args, f"Read Helm {operation}")
        if not output_json:
            return raw
        value = json.loads(raw)
        if value is None:
            return {}
        if not isinstance(value, dict):
            raise ValueError("Helm values must be an object")
        return value

    def revision(self):
        status = json.loads(self.command(
            [*self.helm, "status", self.args.release, *self.release, "--output", "json"],
            "Read Helm status",
        ))
        if status["info"]["status"] != "deployed":
            raise ValueError("Bridge release must be deployed; recover its existing operation first")
        return status["version"]

    def inventory(self, path):
        value = json.loads(self.command(
            [*self.kubectl, "get", "-f", str(path), "--ignore-not-found", "-o", "json"],
            "Read live non-policy resource inventory",
        ))
        objects = value.get("items", []) if value.get("kind") == "List" else [value]
        result = {}
        for obj in objects:
            key = live_key(obj)
            if key in result:
                raise ValueError("Duplicate live resource identity")
            result[key] = obj
        return result

    def configure(self):
        revision = self.revision()
        before = self.helm_read("manifest")
        values = self.helm_read("values", output_json=True)
        network = values.setdefault("networkPolicy", {})
        proxy = network.setdefault("orchestratorProxy", {})
        proxy["enabled"] = not self.args.disable
        if not self.args.disable:
            deadline = time.monotonic() + self.args.timeout
            while not (namespace := self.get("namespace", RUNTIME, optional=True)):
                if time.monotonic() >= deadline:
                    raise RuntimeError("Controller has not created the orchestrator namespace; no changes made")
                time.sleep(2)
            core = (values.get("core") or {}).get("namespace") or "kars-system"
            sandbox = self.get("karssandboxes.kars.azure.com", SANDBOX, core)
            for field, uid in [("namespaceUid", namespace["metadata"]["uid"]),
                               ("sandboxUid", sandbox["metadata"]["uid"])]:
                if proxy.get(field) and proxy[field] != uid:
                    raise ValueError("Recorded orchestrator identity changed; explicit operator review required")
                proxy[field] = uid

        with tempfile.TemporaryDirectory(prefix="kars-bridge-proxy-") as directory:
            identity_path = Path(directory) / "resource-identities.json"
            identities = documents(before)
            identities.pop(("networking.k8s.io/v1", "NetworkPolicy", RUNTIME,
                            self.args.release + "-orchestrator-proxy"), None)
            identity_path.write_text(json.dumps({"apiVersion": "v1", "kind": "List", "items": [
                {"apiVersion": key[0], "kind": key[1], "metadata": {
                    "name": key[3], **({"namespace": key[2]} if key[2] else {}),
                }} for key in identities
            ]}))
            live_before = self.inventory(identity_path)
            require_live_compatible(before, live_before, self.args.release)
            path = Path(directory) / "private-values.json"
            with path.open("x") as stream:
                path.chmod(0o600)
                json.dump(values, stream)
            render = [
                *self.helm, "template", self.args.release, str(CHART), *self.release,
                "--is-upgrade", "--dry-run=server", "--values", str(path),
            ]
            after = self.command(render, "Render and verify live proxy ownership")
            require_policy_only(before, after, self.args.release)
            upgrade = [
                *self.helm, "upgrade", self.args.release, str(CHART), *self.release,
                "--values", str(path), "--timeout", f"{self.args.timeout}s",
            ]
            self.command([*upgrade, "--dry-run=server"], "Server-side Helm preflight")
            if self.revision() != revision or self.helm_read("manifest") != before:
                raise ValueError("Helm release changed after review; no upgrade attempted")
            live_current = self.inventory(identity_path)
            if live_before != live_current:
                raise ValueError("Live release resources changed after preflight; no upgrade attempted")
            if self.args.check:
                return {
                    "helmRevision": revision, "proxyAllowance": "not changed", "preflightPassed": True,
                    "functionalAcceptance": "not performed; no policy or workload changed",
                }
            self.command(upgrade, "Apply proxy-only Helm upgrade", timeout=self.args.timeout + 30)
            require_live_unchanged(live_before, self.inventory(identity_path))

        actual = self.helm_read("manifest")
        require_policy_only(before, actual, self.args.release)
        expected = documents(after)
        if documents(actual) != expected:
            raise RuntimeError("Applied Helm manifest differs from the reviewed render")
        key = ("networking.k8s.io/v1", "NetworkPolicy", RUNTIME,
               self.args.release + "-orchestrator-proxy")
        policy = self.get("networkpolicy", key[-1], RUNTIME, optional=True)
        if self.args.disable:
            if policy is not None:
                raise RuntimeError("Proxy allowance removal has not completed")
        else:
            if policy is None or policy.get("spec") != expected[key]["spec"]:
                raise RuntimeError("Live proxy policy differs from the reviewed rule")
            namespace = self.get("namespace", RUNTIME)
            if namespace["metadata"]["uid"] != proxy["namespaceUid"]:
                raise RuntimeError("Runtime namespace changed during installation")
            self.check_proxy()
        return {
            "helmRevision": self.revision(), "proxyAllowance": "removed" if self.args.disable else "installed",
            "proxyHealth": None if self.args.disable else True,
            "functionalAcceptance": "not performed; authenticate and compose a proposal before declaring readiness",
        }

    def check_proxy(self):
        deadline = time.monotonic() + self.args.timeout
        while time.monotonic() < deadline:
            pods = json.loads(self.command([
                *self.kubectl, "get", "pods", "--namespace", RUNTIME,
                "--selector", "kars.azure.com/sandbox=bridge-orchestrator", "-o", "json",
            ], "Read orchestrator readiness"))["items"]
            ready = [pod for pod in pods if not pod["metadata"].get("deletionTimestamp")
                     and pod.get("status", {}).get("phase") == "Running"
                     and any(c["name"] == "inference-router" and c.get("ready")
                             for c in pod.get("status", {}).get("containerStatuses", []))]
            if ready:
                name = ready[0]["metadata"]["name"]
                result = subprocess.run(
                    [*self.kubectl, "get", "--raw",
                     f"/api/v1/namespaces/{RUNTIME}/pods/{name}:8443/proxy/healthz"],
                    capture_output=True, text=True, timeout=20,
                )
                if result.returncode == 0:
                    return
            time.sleep(2)
        raise RuntimeError(
            "Proxy policy installed, but router health through pods/proxy is unavailable; "
            "composition is not ready. No workloads were restarted."
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kubeconfig", required=True)
    parser.add_argument("--context", required=True)
    parser.add_argument("--namespace", required=True, help="Existing Bridge Helm release namespace")
    parser.add_argument("--release", default="kars-bridge")
    parser.add_argument("--disable", action="store_true", help="Remove only this Bridge-owned allowance")
    parser.add_argument("--check", action="store_true", help="Read-only render and server dry-run; do not upgrade")
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if args.timeout < 1 or args.timeout > 900:
        parser.error("--timeout must be between 1 and 900 seconds")
    try:
        result = Configuration(args).configure()
    except (ValueError, RuntimeError, OSError, subprocess.TimeoutExpired, yaml.YAMLError) as error:
        # Do not echo parser errors, command arguments, or private Helm output.
        message = str(error) if isinstance(error, (ValueError, RuntimeError)) else type(error).__name__
        parser.exit(1, f"Bridge proxy configuration failed: {message}\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
