# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual disposable API Pod-creation proof without executing or pulling images."""

import argparse
import copy
import json
import os
from pathlib import Path
import re
import time

from sre_authority.bootstrap_diagnostics import api_result, collect, controller_stack
from sre_authority.registration_schema import CONTEXT, command, kind_proxy, request, write_report as schema_report

REPORT_PREFIX = ""


def write_report(root, filename, report):
    schema_report(root, REPORT_PREFIX + filename, report)


def failure_site(error):
    result = {"category": type(error).__name__}
    frame = error.__traceback__
    while frame:
        name = Path(frame.tb_frame.f_code.co_filename).name
        if name in ("bootstrap_probe.py", "binding_probe.py"):
            result.update(source=name, line=frame.tb_lineno)
        frame = frame.tb_next
    return result


PATHS = {
    "CustomResourceDefinition": "/apis/apiextensions.k8s.io/v1/customresourcedefinitions",
    "ServiceAccount": "/api/v1/namespaces/{namespace}/serviceaccounts",
    "ClusterRole": "/apis/rbac.authorization.k8s.io/v1/clusterroles",
    "ClusterRoleBinding": "/apis/rbac.authorization.k8s.io/v1/clusterrolebindings",
    "Role": "/apis/rbac.authorization.k8s.io/v1/namespaces/{namespace}/roles",
    "RoleBinding": "/apis/rbac.authorization.k8s.io/v1/namespaces/{namespace}/rolebindings",
    "ValidatingAdmissionPolicy": "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicies",
    "ValidatingAdmissionPolicyBinding": "/apis/admissionregistration.k8s.io/v1/validatingadmissionpolicybindings",
    "Deployment": "/apis/apps/v1/namespaces/{namespace}/deployments",
}


def builtin_documents(rendered):
    documents = []
    for document in re.split(r"(?m)^---\s*\n", rendered):
        kinds = re.findall(r"(?m)^kind:\s*([A-Za-z]+)\s*$", document)
        if len(kinds) == 1 and kinds[0] in PATHS:
            documents.append(document)
    return "\n---\n".join(documents)


def converted_objects(raw):
    # kubectl create -o json emits adjacent JSON objects for a multi-document
    # input, rather than the List returned by kubectl get.
    decoder, objects = json.JSONDecoder(), []
    while raw.strip():
        obj, end = decoder.raw_decode(raw.lstrip())
        raw = raw.lstrip()[end:]
        objects.extend(obj.get("items", []) if obj.get("kind") == "List" else [obj])
    if not objects or any(obj.get("kind") not in PATHS for obj in objects):
        raise RuntimeError("Public bootstrap chart contained unexpected converted objects")
    return objects


def chart(root):
    write_report(root, "bootstrap-stage.json", {"stage": "render-disabled-core"})
    rendered = command("bootstrap-render", [
        "helm", "template", "kars", str(root / "deploy/helm/kars"), "--namespace", "kars-system",
        # Offline rendering cannot satisfy the deliberate live-source Helm lookup.
        # The disabled-core stage emits the same controller and admission policies,
        # without creating or executing an SRE source.
        "--kube-version", "1.31.0", "--set", "sre.enabled=false", "--set", "sre.authorityStage=true",
        "--set", "controller.replicas=1", "--set", "inferenceRouter.replicas=1",
        "--set-string", "inferenceRouter.azure.openai.endpoint=https://e2e-fake.invalid/",
        "--set-string", "foundry.endpoint=https://e2e-fake.invalid/",
        "--set-string", "foundry.projectEndpoint=https://e2e-fake.invalid/",
    ], root=root)
    write_report(root, "bootstrap-stage.json", {"stage": "convert-public-builtins"})
    converted = command("bootstrap-conversion", [
        "kubectl", "--context", CONTEXT, "--request-timeout=15s", "create",
        "--dry-run=client", "--validate=strict", "-f", "-", "-o", "json",
    ], root=root, data=builtin_documents(rendered))
    write_report(root, "bootstrap-stage.json", {"stage": "parse-public-objects"})
    return converted_objects(converted)


def safe_controller(obj):
    if obj.get("kind") != "Deployment" or obj.get("metadata", {}).get("name") != "kars-controller":
        raise AssertionError("Only the public controller template is a bootstrap fixture")
    fixture = copy.deepcopy(obj)
    fixture["metadata"]["namespace"] = "kars-system"
    fixture["spec"]["replicas"] = 1
    pod = fixture["spec"]["template"]["spec"]
    # Admission proof only: no node scheduling, image pulls or container execution.
    pod["schedulerName"] = "kars-e2e-admission-never-schedule"
    for container in pod.get("containers", []) + pod.get("initContainers", []):
        container["image"] = "registry.invalid/kars-admission-proof:never"
        container["imagePullPolicy"] = "Never"
    return fixture


def preserved_json_candidate(objects):
    candidate = copy.deepcopy(objects)
    matches = [obj for obj in candidate if obj["kind"] == "CustomResourceDefinition"
               and obj["metadata"]["name"] == "karssreactions.kars.azure.com"]
    if len(matches) != 1:
        raise RuntimeError("Expected one public SRE action schema")
    schema = matches[0]["spec"]["versions"][0]["schema"]["openAPIV3Schema"]
    params = schema["properties"]["spec"]["properties"]["action"]["properties"]["params"]
    if params.get("type") != "object" or params.get("additionalProperties") is not True:
        raise RuntimeError("API-evidenced boolean additionalProperties candidate precondition failed")
    del params["additionalProperties"]
    params["x-kubernetes-preserve-unknown-fields"] = True
    return candidate


def upsert(port, obj, policies):
    metadata = obj.get("metadata", {})
    path = PATHS[obj["kind"]].format(namespace=metadata.get("namespace", "kars-system"))
    code, existing = request(port, "GET", f"{path}/{metadata['name']}")
    method, accepted = ("PUT", 200) if code == 200 else ("POST", 201)
    if code not in (200, 404):
        return api_result(code, existing, policies)
    desired = copy.deepcopy(obj)
    if method == "PUT":
        desired["metadata"]["resourceVersion"] = existing["metadata"]["resourceVersion"]
        path += "/" + metadata["name"]
    code, body = request(port, method, path, desired)
    result = api_result(code, body, policies)
    result["accepted"] = code == accepted
    result["kind"], result["name"] = obj["kind"], metadata["name"]
    return result


def exercise(root, port, objects, policies, wait_seconds=90, retirement=False):
    results = []
    for name in ("kars-system", "kars-sre"):
        code, body = request(port, "POST", "/api/v1/namespaces", {
            "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": name}})
        if code not in (201, 409):
            raise RuntimeError("Disposable bootstrap namespace unavailable")
    retirement_state = None
    if retirement:
        from sre_authority.binding_probe import seed
        retirement_state = seed(port, root)
    for kind in PATHS:
        if kind == "Deployment":
            continue
        for obj in objects:
            if obj["kind"] != kind:
                continue
            result = upsert(port, obj, policies)
            results.append(result)
            if not result.get("accepted"):
                write_report(root, "bootstrap-install.json", {"operations": results})
                raise RuntimeError("Disposable public bootstrap admission install failed")
    deadline = time.monotonic() + wait_seconds
    while time.monotonic() < deadline:
        snapshot = collect(port, policies, request)
        if all(entry.get("typeChecked") and entry.get("generation") == entry.get("observedGeneration")
               for entry in snapshot["policies"]):
            break
        time.sleep(1)
    else:
        write_report(root, "bootstrap-before-create.json", snapshot)
        # Still collect actual Pod/ReplicaSet admission evidence after the
        # bounded observation wait. An unrelated policy must not hide the
        # creation failure; the unobserved-policy gate remains fatal below.
    observed = all(entry.get("typeChecked") and entry.get("generation") == entry.get("observedGeneration")
                   for entry in snapshot["policies"])
    write_report(root, "bootstrap-before-create.json", snapshot)
    templates = [obj for obj in objects if obj["kind"] == "Deployment"
                 and obj["metadata"]["name"] == "kars-controller"]
    if len(templates) != 1:
        raise RuntimeError("Expected one public controller Deployment")
    fixture = safe_controller(templates[0])
    template = fixture["spec"]["template"]
    pod = {"apiVersion": "v1", "kind": "Pod",
           "metadata": {"name": "kars-controller-admission-direct", "namespace": "kars-system",
                        "labels": template["metadata"]["labels"]}, "spec": template["spec"]}
    code, body = request(port, "POST", "/api/v1/namespaces/kars-system/pods?dryRun=All", pod)
    direct = api_result(code, body, policies)
    result = upsert(port, fixture, policies)
    write_report(root, "bootstrap-create.json", {"directPodDryRun": direct, "deployment": result})
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        snapshot = collect(port, policies, request)
        if any(obj.get("kind") == "Pod" for obj in snapshot["workloads"]):
            break
        time.sleep(1)
    write_report(root, "bootstrap-after-create.json", snapshot)
    if code != 201 or not result.get("accepted") or not any(obj.get("kind") == "Pod" for obj in snapshot["workloads"]):
        raise RuntimeError("Real controller Deployment/ReplicaSet did not create an admission-only Pod")
    write_report(root, "bootstrap-result.json", {"podCreation": "accepted",
                 "workloadExecution": "not-attempted", "readiness": "not-claimed",
                 "allPoliciesObservedBeforeCreate": observed})
    if not observed:
        raise RuntimeError("Public admission policy observation timed out; Pod evidence was still collected")
    return retirement_state


def main(root, diagnostics_only, candidate=False, retirement=False):
    global REPORT_PREFIX
    REPORT_PREFIX = "candidate-" if candidate else ""
    with kind_proxy(root) as (port, version):
        write_report(root, "bootstrap-versions.json", {"apiServer": version, "context": CONTEXT})
        objects = chart(root)
        if candidate:
            objects = preserved_json_candidate(objects)
        policies = {obj["metadata"]["name"]: obj for obj in objects
                    if obj["kind"] == "ValidatingAdmissionPolicy"}
        if not policies:
            raise RuntimeError("Public admission policies were not rendered")
        if diagnostics_only:
            write_report(root, "bootstrap-install-failure.json", collect(port, policies, request))
            write_report(root, "bootstrap-controller-stack.json", controller_stack(
                CONTEXT, "kube-controller-manager-kars-e2e-control-plane"))
        else:
            try:
                state = exercise(root, port, objects, policies, wait_seconds=180 if candidate else 90,
                                 retirement=retirement and not candidate)
                from sre_authority.bootstrap_cases import admission_cases
                cases = admission_cases(port, policies)
                write_report(root, "bootstrap-admission-cases.json", {"cases": cases})
                if not all(case["matched"] for case in cases):
                    raise RuntimeError("Schema failed intended ordinary/private admission outcomes")
                if state:
                    from sre_authority.binding_probe import prove
                    prove(root, port, state, objects,
                          lambda facts: write_report(root, "bootstrap-binding-retirement.json", facts))
            finally:
                write_report(root, "bootstrap-final.json", collect(port, policies, request))
                write_report(root, "bootstrap-controller-stack.json", controller_stack(
                    CONTEXT, "kube-controller-manager-kars-e2e-control-plane"))


if __name__ == "__main__":
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--diagnostics-only", action="store_true")
    parser.add_argument("--json-params-candidate", action="store_true",
                        help="Diagnose only the API-evidenced nil-schema adapter candidate; never edit production")
    parser.add_argument("--retirement-bind-proof", action="store_true",
                        help="Prove controller-principal retirement dry-runs with a test-only exact reader bind grant")
    args = parser.parse_args()
    try:
        main(Path(__file__).resolve().parents[3], args.diagnostics_only, args.json_params_candidate,
             args.retirement_bind_proof)
    except Exception as error:
        print(f"SRE-BOOTSTRAP-FAIL category={type(error).__name__}", flush=True)
        write_report(Path(__file__).resolve().parents[3], "bootstrap-failure-site.json", failure_site(error))
        raise SystemExit(1) from None
