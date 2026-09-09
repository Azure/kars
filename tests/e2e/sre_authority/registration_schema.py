# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real API validation for one public CRD; never a generic error-body logger."""

import contextlib
import argparse
import copy
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import Request, build_opener, ProxyHandler

CONTEXT = "kind-kars-e2e"
CRD_NAME = "karssreregistrations.kars.azure.com"
CRD_PATH = "/apis/apiextensions.k8s.io/v1/customresourcedefinitions"
TEMPLATE = "templates/crd-karssreregistration.yaml"
REPORT_DIR = "e2e-sre-schema-diag"
ORIGINAL_RULE = "self.sandbox.namespace == self.controller.namespace.name"
ESCAPED_RULE = "self.sandbox.__namespace__ == self.controller.__namespace__.name"


def require_public_crd(obj):
    if (not isinstance(obj, dict) or obj.get("apiVersion") != "apiextensions.k8s.io/v1"
            or obj.get("kind") != "CustomResourceDefinition"
            or obj.get("metadata", {}).get("name") != CRD_NAME
            or obj.get("spec", {}).get("group") != "kars.azure.com"
            or obj.get("spec", {}).get("scope") != "Cluster"
            or obj.get("spec", {}).get("names", {}).get("kind") != "KarsSRERegistration"
            or obj.get("spec", {}).get("names", {}).get("plural") != "karssreregistrations"
            or set(obj) - {"apiVersion", "kind", "metadata", "spec"}):
        raise AssertionError("Schema diagnostic accepts only the public SRE registration CRD")


def public_status(code, body):
    """Only this known public schema's Invalid causes may cross the log boundary."""
    report = {"resource": CRD_NAME, "httpStatus": code, "category": "unexpected-response"}
    if not isinstance(body, dict):
        return report
    if code in (200, 201) and body.get("kind") == "CustomResourceDefinition":
        if body.get("metadata", {}).get("name") == CRD_NAME:
            report["category"] = "accepted"
        return report
    reasons = {"Invalid", "Forbidden", "Unauthorized", "NotFound", "AlreadyExists",
               "Conflict", "BadRequest", "InternalError", "ServiceUnavailable"}
    if body.get("kind") == "Status" and body.get("reason") in reasons:
        report["category"] = body["reason"]
    details = body.get("details", {})
    if (code != 422 or body.get("kind") != "Status" or body.get("reason") != "Invalid"
            or not isinstance(details, dict) or details.get("name") != CRD_NAME
            or details.get("group") != "apiextensions.k8s.io"
            or details.get("kind") != "CustomResourceDefinition"):
        return report
    causes = []
    supplied = details.get("causes", [])
    if not isinstance(supplied, list):
        return report
    for cause in supplied[:32]:
        if not isinstance(cause, dict):
            continue
        field, message = cause.get("field"), cause.get("message")
        if (not isinstance(field, str) or not field.startswith("spec.")
                or not isinstance(message, str)):
            continue
        causes.append({
            "field": re.sub(r"[\x00-\x1f\x7f]", "?", field)[:1024],
            "message": re.sub(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]", "?", message)[:16384],
        })
    report["causes"] = causes
    return report


def write_report(root, filename, report):
    directory = Path(root) / REPORT_DIR
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    (directory / filename).write_text(json.dumps(report, indent=2) + "\n")
    print("SRE-CRD-SCHEMA " + json.dumps(report, sort_keys=True), flush=True)


def create_registration_crd(harness, obj):
    require_public_crd(obj)
    response = harness.api("POST", CRD_PATH, body=obj)
    try:
        body = response.json()
    except (ValueError, TypeError):
        body = None
    report = public_status(response.status_code, body)
    write_report(harness.root, "registration-create.json", report)
    if response.status_code != 201 or report["category"] != "accepted":
        raise AssertionError(
            f"Public SRE registration CRD rejected: HTTP {response.status_code}; "
            f"category={report['category']}; see allowlisted schema causes"
        )
    return body


def command(stage, args, *, root, data=None):
    try:
        result = subprocess.run(args, cwd=root, input=data, text=True, capture_output=True,
                                timeout=45, check=False)
    except (OSError, subprocess.TimeoutExpired):
        raise RuntimeError(f"Public schema preflight {stage} command unavailable/timed out") from None
    if result.returncode:
        raise RuntimeError(f"Public schema preflight {stage} failed; exit={result.returncode}") from None
    return result.stdout


def request(port, method, path, obj=None, *, content_type="application/json"):
    body = None if obj is None else json.dumps(obj).encode()
    req = Request(f"http://127.0.0.1:{port}{path}", data=body, method=method,
                  headers={"Content-Type": content_type, "Accept": "application/json"})
    opener = build_opener(ProxyHandler({}))
    try:
        response = opener.open(req, timeout=15)
    except HTTPError as error:
        response = error
    with response:
        code = response.code
        raw = response.read(1024 * 1024)
    try:
        return code, json.loads(raw)
    except (ValueError, TypeError):
        return code, None


@contextlib.contextmanager
def kind_proxy(root):
    # Read only redacted config to verify the exact disposable context/server.
    config = json.loads(command("context", ["kubectl", "--context", CONTEXT, "config", "view",
                                          "--minify", "-o", "json"], root=root))
    contexts, clusters = config.get("contexts", []), config.get("clusters", [])
    if (len(contexts) != 1 or contexts[0].get("name") != CONTEXT or len(clusters) != 1
            or urlsplit(clusters[0].get("cluster", {}).get("server", "")).hostname
            not in ("localhost", "127.0.0.1", "::1")):
        raise RuntimeError("Public schema preflight refuses a non-loopback/non-Kind context")
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    process = subprocess.Popen(
        ["kubectl", "--context", CONTEXT, "--request-timeout=15s", "proxy",
         "--address=127.0.0.1", f"--port={port}"],
        cwd=root, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        deadline = time.monotonic() + 25
        version = None
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RuntimeError("Public schema preflight proxy exited before readiness")
            try:
                code, observed = request(port, "GET", "/version")
                if code == 200 and isinstance(observed, dict) and isinstance(observed.get("gitVersion"), str):
                    version = {key: observed.get(key) for key in ("major", "minor", "gitVersion")}
                    break
            except (URLError, TimeoutError):
                pass
            time.sleep(0.2)
        if version is None:
            raise RuntimeError("Public schema preflight proxy readiness timed out")
        yield port, version
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def escaped_namespace_candidate(obj):
    """Diagnostic-only candidate: preserve the constraint and its wire schema."""
    candidate = copy.deepcopy(obj)
    rules = candidate["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["x-kubernetes-validations"]
    matches = [rule for rule in rules if rule.get("rule") == ORIGINAL_RULE]
    if len(matches) != 1:
        raise RuntimeError("Expected public namespace equality rule is absent; candidate probe refused")
    matches[0]["rule"] = ESCAPED_RULE
    return candidate


def exercise_instances(root, port, obj, method, path, accepted, prefix):
    # This helper is used only in the isolated schema CI job. The full harness
    # keeps its preflight dry-run-only so historical fixture setup is unchanged.
    code, body = request(port, method, path, obj)
    if code != accepted or public_status(code, body)["category"] != "accepted":
        write_report(root, f"{prefix}-install.json", public_status(code, body))
        raise RuntimeError("Public schema could not be installed in the disposable schema cluster")
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        code, current = request(port, "GET", f"{CRD_PATH}/{CRD_NAME}")
        if code == 200 and any(condition.get("type") == "Established" and condition.get("status") == "True"
                               for condition in current.get("status", {}).get("conditions", [])):
            break
        time.sleep(0.5)
    else:
        raise RuntimeError("Public registration CRD did not become Established")
    instance = {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsSRERegistration",
        "metadata": {"name": "canonical"},
        "spec": {
            "controller": {"namespace": {"name": "kars-system", "uid": "fixture-system"},
                           "deployment": {"name": "kars-controller", "uid": "fixture-controller"}, "release": "kars"},
            "sandbox": {"namespace": "kars-system", "name": "sre", "uid": "fixture-source"},
            "runtimeNamespace": {"name": "kars-sre", "uid": "fixture-runtime"},
        },
    }
    results = []
    for label, expected, message in [
        ("canonical-matching-namespace", 201, None),
        ("foreign-source-namespace", 422, "SRE must be registered in its controller/release namespace"),
        ("noncanonical-name", 422, "The canonical SRE registration is the only supported instance"),
    ]:
        probe = copy.deepcopy(instance)
        if label == "foreign-source-namespace":
            probe["spec"]["sandbox"]["namespace"] = "other-system"
        elif label == "noncanonical-name":
            probe["metadata"]["name"] = "other"
        code, response = request(port, "POST", "/apis/kars.azure.com/v1alpha1/karssreregistrations?dryRun=All", probe)
        matched = code == expected
        if message:
            matched = matched and isinstance(response, dict) and response.get("reason") == "Invalid" and any(
                message in cause.get("message", "") for cause in response.get("details", {}).get("causes", [])
            )
        else:
            matched = matched and isinstance(response, dict) and response.get("kind") == "KarsSRERegistration"
        results.append({"case": label, "httpStatus": code, "expectedStatus": expected, "matched": bool(matched)})
        write_report(root, f"{prefix}-instances.json", {"cases": results})
        if not matched:
            raise RuntimeError("Public registration instance did not satisfy the exact expected schema invariant")


def preflight(root, *, candidate=False, exercise=False):
    rendered = command("render", ["helm", "template", "kars", str(root / "deploy/helm/kars"),
                                 "--namespace", "kars-system", "--show-only", TEMPLATE], root=root)
    # Existing kubectl parses YAML; strict client validation remains enabled.
    obj = json.loads(command("conversion", [
        "kubectl", "--context", CONTEXT, "--request-timeout=15s", "create",
        "--dry-run=client", "--validate=strict", "-f", "-", "-o", "json",
    ], root=root, data=rendered))
    require_public_crd(obj)
    if candidate:
        obj = escaped_namespace_candidate(obj)
    prefix = "namespace-accessor-candidate" if candidate else "validation"
    with kind_proxy(root) as (port, version):
        write_report(root, "versions.json", {"apiServer": version, "context": CONTEXT})
        code, existing = request(port, "GET", f"{CRD_PATH}/{CRD_NAME}")
        if code == 404:
            method, path, accepted = "POST", CRD_PATH, 201
        elif code == 200 and public_status(code, existing)["category"] == "accepted":
            method, path, accepted = "PUT", f"{CRD_PATH}/{CRD_NAME}", 200
            for key in ("uid", "resourceVersion"):
                obj["metadata"][key] = existing["metadata"][key]
        else:
            raise RuntimeError(f"Public schema preflight could not inspect CRD; HTTP {code}")
        code, body = request(port, method, path + "?dryRun=All", obj)
        report = public_status(code, body)
        write_report(root, f"{prefix}.json", report)
        if code != accepted or report["category"] != "accepted":
            raise RuntimeError(f"Public SRE registration schema rejected; HTTP {code}")
        if exercise:
            exercise_instances(root, port, obj, method, path, accepted, prefix)


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--namespace-accessor-candidate", action="store_true",
                        help="Probe only the API-evidenced field-accessor candidate; does not edit production files")
    parser.add_argument("--exercise", action="store_true", help="Exercise public instance invariants in the isolated schema job")
    args = parser.parse_args()
    try:
        preflight(Path(__file__).resolve().parents[3],
                  candidate=args.namespace_accessor_candidate, exercise=args.exercise)
    except Exception as error:
        # Deliberately do not expose arbitrary exception text, command output,
        # argv, config material, or an unrelated HTTP response.
        print(f"SRE-CRD-SCHEMA-FAIL category={type(error).__name__}", flush=True)
        raise SystemExit(1) from None
