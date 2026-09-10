# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Admission-only proof using the live KarsEval lifecycle's two Pod templates."""

import argparse
import copy
import json
from pathlib import Path
import re
import time
from urllib.error import URLError
import uuid

from sre_authority import registration_schema as api

SOURCE_NAMESPACE = "kars-system"
EVAL_NAME = "e2e-karseval-lc"
CRON_NAME = f"karseval-{EVAL_NAME}"
NAMESPACES = "/api/v1/namespaces"
PROOF_LABEL = "kars.azure.com/eval-admission-proof"
PSS_LABELS = {
    "pod-security.kubernetes.io/enforce": "restricted",
    "pod-security.kubernetes.io/enforce-version": "v1.31",
}
PSS_FAILURES = (
    "allowPrivilegeEscalation != false", "unrestricted capabilities",
    "runAsNonRoot != true", "seccompProfile",
)


class ProbeFailure(RuntimeError):
    """Only fixed stage names, never resource contents or API error messages."""


def require(condition, stage):
    if not condition:
        raise ProbeFailure(stage)


def status_response(code, body, expected, reason, name, kind):
    return (
        code == expected and isinstance(body, dict)
        and body.get("apiVersion") == "v1" and body.get("kind") == "Status"
        and body.get("status") == "Failure" and body.get("code") == expected
        and body.get("reason") == reason and isinstance(body.get("details"), dict)
        and body["details"].get("name") == name and body["details"].get("kind") == kind
    )


def metadata(obj, kind, name, namespace, *, generation=False, terminating=False):
    require(isinstance(obj, dict) and obj.get("kind") == kind, "resource-kind")
    meta = obj.get("metadata")
    require(isinstance(meta, dict) and meta.get("name") == name
            and meta.get("namespace") == namespace, "resource-location")
    require(all(isinstance(meta.get(key), str) and meta[key] for key in ("uid", "resourceVersion")),
            "resource-identity")
    require(terminating or meta.get("deletionTimestamp") is None, "resource-terminating")
    if generation:
        require(type(meta.get("generation")) is int and meta["generation"] > 0,
                "resource-generation")
    return meta


def source_snapshot(obj):
    meta = obj["metadata"]
    return copy.deepcopy({
        "apiVersion": obj["apiVersion"], "kind": obj["kind"], "spec": obj["spec"],
        "metadata": {key: meta.get(key) for key in (
            "name", "namespace", "uid", "generation", "ownerReferences", "deletionTimestamp",
        )},
    })


def read_source(port, path, kind, name):
    code, obj = api.request(port, "GET", path)
    require(code == 200, "source-read")
    metadata(obj, kind, name, SOURCE_NAMESPACE, generation=True)
    version = "kars.azure.com/v1alpha1" if kind == "KarsEval" else "batch/v1"
    require(obj.get("apiVersion") == version and isinstance(obj.get("spec"), dict),
            "source-shape")
    return obj


def sources(port, job_name):
    require(isinstance(job_name, str) and re.fullmatch(
        rf"karseval-{EVAL_NAME}-runnow-[0-9a-f]{{10}}", job_name), "job-name")
    descriptors = [
        (f"/apis/kars.azure.com/v1alpha1/namespaces/{SOURCE_NAMESPACE}/karsevals/{EVAL_NAME}",
         "KarsEval", EVAL_NAME),
        (f"/apis/batch/v1/namespaces/{SOURCE_NAMESPACE}/jobs/{job_name}", "Job", job_name),
        (f"/apis/batch/v1/namespaces/{SOURCE_NAMESPACE}/cronjobs/{CRON_NAME}", "CronJob", CRON_NAME),
    ]
    objects = [read_source(port, *descriptor) for descriptor in descriptors]
    evaluation, job, cron = objects
    require(evaluation["spec"].get("targetSandboxRef") == {"name": "e2e-test"}
            and evaluation["spec"].get("corpus") == {"builtin": "jailbreak-baseline"}
            and evaluation["spec"].get("schedule") == "*/15 * * * *"
            and cron["spec"].get("schedule") == evaluation["spec"]["schedule"], "lifecycle-source")
    owner = {
        "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsEval", "name": EVAL_NAME,
        "uid": evaluation["metadata"]["uid"], "controller": True, "blockOwnerDeletion": True,
    }
    for producer in (job, cron):
        require(producer["metadata"].get("ownerReferences") == [owner], "producer-owner")
        labels = producer["metadata"].get("labels", {})
        require(isinstance(labels, dict)
                and labels.get("app.kubernetes.io/managed-by") == "kars-controller"
                and labels.get("kars.azure.com/karseval") == EVAL_NAME, "producer-labels")
    job_template = job["spec"].get("template")
    cron_job = cron["spec"].get("jobTemplate", {})
    require(isinstance(cron_job, dict) and isinstance(cron_job.get("spec"), dict),
            "cron-template")
    cron_template = cron_job["spec"].get("template")
    for template in (job_template, cron_template):
        require(isinstance(template, dict) and isinstance(template.get("spec"), dict)
                and isinstance(template.get("metadata", {}), dict), "pod-template")
        spec = template["spec"]
        containers = spec.get("containers")
        require(isinstance(containers, list) and len(containers) == 1
                and isinstance(containers[0], dict) and containers[0].get("name") == "runner",
                "runner-container")
        args = containers[0].get("args")
        require(isinstance(args, list) and len(args) == 6
                and args[:3] == ["--corpus", "/etc/kars/eval-corpus/corpus.json", "--router-base"]
                and args[3] == "http://e2e-test.kars-e2e-test.svc.cluster.local:8443"
                and args[4:] == ["--output", "/dev/stdout"]
                and not any(arg.startswith("--corpus-label") for arg in args), "runner-arguments")
    require(job_template["spec"] == cron_template["spec"], "producer-spec-parity")
    return descriptors, objects, (job_template, cron_template)


def unchanged_sources(port, descriptors, originals):
    for descriptor, original in zip(descriptors, originals):
        require(source_snapshot(read_source(port, *descriptor)) == source_snapshot(original),
                "source-continuity")


def namespace_identity(obj, name, nonce, *, terminating=False):
    meta = metadata(obj, "Namespace", name, None, terminating=terminating)
    labels = meta.get("labels")
    require(obj.get("apiVersion") == "v1" and isinstance(labels, dict)
            and labels.get(PROOF_LABEL) == nonce
            and all(labels.get(key) == value for key, value in PSS_LABELS.items())
            and not meta.get("ownerReferences") and isinstance(obj.get("spec"), dict),
            "namespace-ownership")
    return meta


def current_namespace(port, original, nonce, *, terminating=False):
    name = original["metadata"]["name"]
    code, current = api.request(port, "GET", f"{NAMESPACES}/{name}")
    require(code == 200, "namespace-read")
    meta = namespace_identity(current, name, nonce, terminating=terminating)
    require(meta["uid"] == original["metadata"]["uid"] and current["spec"] == original["spec"]
            and meta.get("labels") == original["metadata"].get("labels"),
            "namespace-continuity")
    return current


def create_namespace(port, name, nonce):
    code, body = api.request(port, "GET", f"{NAMESPACES}/{name}")
    require(status_response(code, body, 404, "NotFound", name, "namespaces"),
            "namespace-not-fresh")
    obj = {"apiVersion": "v1", "kind": "Namespace",
           "metadata": {"name": name, "labels": {**PSS_LABELS, PROOF_LABEL: nonce}}}
    code, created = api.request(port, "POST", NAMESPACES, obj)
    require(code == 201, "namespace-create")
    namespace_identity(created, name, nonce)
    return created


def wait_for_service_account(port, original, nonce):
    name = original["metadata"]["name"]
    deadline = time.monotonic() + 30
    while True:
        current_namespace(port, original, nonce)
        code, body = api.request(port, "GET", f"{NAMESPACES}/{name}/serviceaccounts/default")
        if code == 200:
            metadata(body, "ServiceAccount", "default", name)
            require(body.get("apiVersion") == "v1", "service-account-shape")
            return
        require(status_response(code, body, 404, "NotFound", "default", "serviceaccounts"),
                "service-account-read")
        require(time.monotonic() < deadline, "service-account-timeout")
        time.sleep(0.2)


def cleanup_namespace(port, original, nonce):
    name = original["metadata"]["name"]
    current = current_namespace(port, original, nonce)
    meta = current["metadata"]
    code, response = api.request(port, "DELETE", f"{NAMESPACES}/{name}", {
        "apiVersion": "v1", "kind": "DeleteOptions",
        "preconditions": {"uid": meta["uid"], "resourceVersion": meta["resourceVersion"]},
    })
    require(code in (200, 202), "namespace-delete")
    # Namespace deletion returns the terminating Namespace, not arbitrary JSON
    # with a successful HTTP status.
    returned = namespace_identity(response, name, nonce, terminating=True)
    require(returned["uid"] == meta["uid"] and response["spec"] == original["spec"],
            "namespace-delete-identity")
    deadline = time.monotonic() + 30
    while True:
        code, body = api.request(port, "GET", f"{NAMESPACES}/{name}")
        if status_response(code, body, 404, "NotFound", name, "namespaces"):
            return
        require(code == 200, "namespace-delete-observation")
        returned = namespace_identity(body, name, nonce, terminating=True)
        require(returned["uid"] == meta["uid"] and body["spec"] == original["spec"],
                "namespace-delete-replacement")
        require(time.monotonic() < deadline, "namespace-delete-timeout")
        time.sleep(0.2)


def pod_from_template(template, namespace, name, *, legacy=False):
    pod = copy.deepcopy(template)
    pod.update({"apiVersion": "v1", "kind": "Pod"})
    meta = pod.setdefault("metadata", {})
    meta.update({"name": name, "namespace": namespace})
    meta.pop("generateName", None)
    if legacy:
        pod["spec"].pop("securityContext", None)
        for field in ("containers", "initContainers", "ephemeralContainers"):
            for container in pod["spec"].get(field, []):
                container.pop("securityContext", None)
    return pod


def contains_spec(actual, expected):
    """Admission may add defaults; every submitted spec value must survive."""
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(
            key in actual and contains_spec(actual[key], value) for key, value in expected.items())
    if isinstance(expected, list):
        return isinstance(actual, list) and len(actual) >= len(expected) and all(
            contains_spec(a, b) for a, b in zip(actual, expected))
    return type(actual) is type(expected) and actual == expected


def admission_result(code, body, pod, legacy):
    name, namespace = pod["metadata"]["name"], pod["metadata"]["namespace"]
    if legacy:
        require(status_response(code, body, 403, "Forbidden", name, "pods"), "legacy-denial-status")
        message = body.get("message")
        require(isinstance(message, str) and message.startswith(
            f'pods "{name}" is forbidden: violates PodSecurity "restricted:v1.31":')
            and all(fragment in message for fragment in PSS_FAILURES), "legacy-denial-pod-security")
        return "PodSecurityForbidden"
    require(code == 201 and isinstance(body, dict) and body.get("apiVersion") == "v1"
            and body.get("kind") == "Pod" and isinstance(body.get("metadata"), dict)
            and body["metadata"].get("name") == name
            and body["metadata"].get("namespace") == namespace
            and contains_spec(body.get("spec"), pod["spec"]), "produced-pod-admission")
    return "accepted"


def probe(port, job_name):
    descriptors, originals, templates = sources(port, job_name)
    unchanged_sources(port, descriptors, originals)
    nonce = uuid.uuid4().hex
    name = f"kars-eval-admission-{nonce}"
    namespace = create_namespace(port, name, nonce)
    results = []
    try:
        wait_for_service_account(port, namespace, nonce)
        for producer, template in zip(("Job", "CronJob"), templates):
            for legacy in (False, True):
                unchanged_sources(port, descriptors, originals)
                current_namespace(port, namespace, nonce)
                pod = pod_from_template(template, name, f"{producer.lower()}-{'legacy' if legacy else 'current'}",
                                        legacy=legacy)
                code, body = api.request(port, "POST", f"{NAMESPACES}/{name}/pods?dryRun=All", pod)
                result = admission_result(code, body, pod, legacy)
                results.append({"producer": producer, "variant": "legacy" if legacy else "current",
                                "httpStatus": code, "result": result})
                unchanged_sources(port, descriptors, originals)
    finally:
        cleanup_namespace(port, namespace, nonce)
    unchanged_sources(port, descriptors, originals)
    return results


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--job", required=True)
    args = parser.parse_args(argv)
    try:
        root = Path(__file__).resolve().parents[2]
        with api.kind_proxy(root) as (port, version):
            require(isinstance(version, dict) and isinstance(version.get("gitVersion"), str)
                    and re.fullmatch(r"v1\.31\.\d+(?:[-+].*)?", version["gitVersion"]),
                    "kind-api-version")
            results = probe(port, args.job)
    except (RuntimeError, ValueError, TypeError, OSError, URLError) as error:
        stage = str(error) if isinstance(error, ProbeFailure) else "kind-api-transport"
        print("KARSEVAL-POD-ADMISSION " + json.dumps({"result": "failed", "stage": stage}), flush=True)
        return 1
    print("KARSEVAL-POD-ADMISSION " + json.dumps({
        "result": "passed", "policy": "restricted:v1.31", "cases": results,
    }, sort_keys=True), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
