# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Failure-time rotation facts; never retries, mutates, or reads Secret data."""

from datetime import datetime, timezone
import json
import re
import subprocess
import time
from types import SimpleNamespace
from urllib.parse import urlencode

from api_outcome_diagnostics import timestamp
from native_api import CORE, ROOT, require, core, resource
from observation_diagnostics import READ_ERRORS, identity, recheck_actor, resolve_actor
from observer_network_diagnostics import complete_inventory, metadata, selected_origin

FAILURE = f"Deadline: current native grant and writer in {CORE}"
UNAVAILABLE = "Rotation snapshot unavailable"
ERRORS = READ_ERRORS + (AttributeError, IndexError)
LABEL = "kars.azure.com/observer-metadata-grant"
OWNER = "kars.azure.com/credential-grant-owner"
TERMINATION_REASONS = frozenset(("Completed", "Error", "OOMKilled", "ContainerCannotRun",
                               "StartError", "DeadlineExceeded", "Evicted"))
ERROR_STAGES = {
    "Discover observer Cilium policy API": "cilium-discovery",
    "Read namespaced observer API policies for retirement": "cilium-retirement-read",
    "Retire owned observer API policy": "cilium-retirement-delete",
    "Replace owned observer network policy": "policy-replace",
    "Publish credential authority": "grant-publication",
    "Read credential grants": "grant-list",
}


def integer(value, minimum=0):
    require(type(value) is int and minimum <= value < 2**31, UNAVAILABLE)
    return value


class _Reader:
    def __init__(self, admin):
        self.admin, self.deadline, self.calls = admin, time.monotonic() + 40, 0

    def remaining(self):
        remaining = self.deadline - time.monotonic()
        require(remaining > 0 and self.calls < 48, UNAVAILABLE)
        return min(10, remaining)

    def request(self, path, **kwargs):
        timeout = self.remaining()
        self.calls += 1
        return self.admin.request("GET", path, timeout=timeout, **kwargs)

    def get(self, path):
        return self.request(path)[1]


def lifecycle_metadata(value):
    fields = value["metadata"]
    deletion = fields.get("deletionTimestamp")
    require(deletion is None or timestamp(deletion) is not None, UNAVAILABLE)
    projected = metadata({"metadata": {key: item for key, item in fields.items()
                                      if key != "deletionTimestamp"}})
    finalizers = fields.get("finalizers", [])
    require(isinstance(finalizers, list) and len(finalizers) <= 32
            and all(isinstance(item, str) for item in finalizers), UNAVAILABLE)
    return {**projected, "deleting": deletion is not None, "finalizerCount": len(finalizers)}


def metadata_read(reader, path, listed=False):
    kind = "PartialObjectMetadataList" if listed else "PartialObjectMetadata"
    code, value = reader.request(path, expected=(200, 404),
                                accept=f"application/json;as={kind};g=meta.k8s.io;v=v1")
    if code == 404:
        require(not listed, UNAVAILABLE)
        return None
    require(isinstance(value, dict) and value.get("kind") == kind
            and value.get("apiVersion") == "meta.k8s.io/v1"
            and set(value) <= {"kind", "apiVersion", "metadata", "items"}, UNAVAILABLE)
    if listed:
        values = complete_inventory(value, 32)
        require(all(isinstance(item, dict) and set(item) <= {"kind", "apiVersion", "metadata"}
                    and isinstance(item.get("metadata"), dict) for item in values), UNAVAILABLE)
        return values
    require("items" not in value, UNAVAILABLE)
    return value


def controller_pod(pod):
    statuses = pod["status"]["containerStatuses"]
    require(isinstance(statuses, list) and len(statuses) <= 4, UNAVAILABLE)
    matches = [item for item in statuses if item.get("name") == "controller"]
    require(len(matches) == 1 and type(matches[0].get("ready")) is bool, UNAVAILABLE)
    status = matches[0]
    result = {"identity": metadata(pod), "ready": status["ready"],
              "restartCount": integer(status["restartCount"]), "lastTermination": {"status": "unrecorded"}}
    last = status.get("lastState", {})
    require(isinstance(last, dict), UNAVAILABLE)
    if "terminated" in last:
        terminal = last["terminated"]
        reason = terminal.get("reason")
        result["lastTermination"] = {
            "status": "recorded", "reason": reason if reason in TERMINATION_REASONS else "other",
            "exitCode": integer(terminal["exitCode"], -(2**31)),
            "signal": integer(terminal["signal"]) if "signal" in terminal else None,
        }
    return result


def project_warnings(raw, since, until):
    require(isinstance(raw, bytes) and len(raw) <= 65536, UNAVAILABLE)
    records = []
    for line in raw.splitlines()[-128:]:
        if len(line) > 8192:
            continue
        try:
            event = json.loads(line)
            at = timestamp(event.get("timestamp"))
            fields = event["fields"]
            if at is None or not since <= at <= until:
                continue
            if (event.get("target") == "kars_controller"
                    and fields.get("message") == "Credential grant controller stopped"):
                category, status = "controller-task-stopped", None
            elif (event.get("target") == "kars_controller::credential_grants"
                  and fields.get("message") in ("Credential grant is not ready", "Credential authority unavailable")):
                category, status = "unclassified", None
                error = fields.get("error")
                if isinstance(error, str) and len(error) <= 4096:
                    for prefix, name in ERROR_STAGES.items():
                        matched = re.fullmatch(re.escape(prefix) + r": Kubernetes status ([1-5][0-9]{2})", error)
                        if matched:
                            category, status = name, int(matched[1])
                    if error == "Observer API policy retirement is pending":
                        category = "cilium-retirement-pending"
            else:
                continue
            records.append({"category": category, "httpStatus": status,
                            "workspaceMatches": (fields["namespace"] in (CORE, f'Some("{CORE}")')
                                                 if isinstance(fields.get("namespace"), str) else None),
                            "afterCaseStartMs": round((at - since).total_seconds() * 1000)})
        except ERRORS:
            continue
    return records[-16:]


def warnings(setup, reader, pod, since):
    selected_origin(setup)
    require(isinstance(since, datetime) and since.tzinfo is not None
            and 0 <= (datetime.now(timezone.utc) - since).total_seconds() <= 600, UNAVAILABLE)
    result = subprocess.run(
        ["kubectl", "--context", "kind-bridge-native", "--request-timeout=10s", "logs",
         "-n", CORE, pod["name"], "-c", "controller", "--tail=128", "--limit-bytes=65536",
         "--since-time=" + since.isoformat()],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        timeout=reader.remaining(), check=False)
    require(result.returncode == 0, UNAVAILABLE)
    records = project_warnings(result.stdout, since, datetime.now(timezone.utc))
    return {"status": "observed" if records else "unobserved", "records": records,
            "podUid": pod["uid"], "coverage": "first-bound-controller-pod", "reconcileStage": "unavailable"}


def bound_metadata(value, namespace):
    result = lifecycle_metadata(value)
    owners = value["metadata"].get("ownerReferences", [])
    require(result.get("namespace") == namespace["metadata"]["name"]
            and isinstance(owners, list) and len(owners) == 1
            and owners[0].get("apiVersion") == "v1" and owners[0].get("kind") == "Namespace"
            and owners[0].get("name") == namespace["metadata"]["name"]
            and owners[0].get("uid") == identity(namespace)[0], UNAVAILABLE)
    return result


def observer_metadata(reader, target, grant):
    require(isinstance(target.get("sandbox"), str)
            and re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}", target["sandbox"]), UNAVAILABLE)
    source_path = resource(CORE, "karssandboxes", target["sandbox"])
    source = reader.get(source_path)
    require(metadata(source)["uid"] == target["uid"] and source["metadata"].get("namespace") == CORE
            and source["metadata"].get("name") == target["sandbox"], UNAVAILABLE)
    name = f"kars-{target['sandbox']}"
    namespace = reader.get(f"/api/v1/namespaces/{name}")
    annotations = namespace["metadata"].get("annotations", {})
    observed = source.get("status", {}).get("serviceObservation") or {}
    expected_uids = [value for value in (
        source["metadata"].get("annotations", {}).get("kars.azure.com/namespace-uid"),
        observed.get("namespaceUid")) if value is not None]
    require(metadata(namespace)["name"] == name
            and annotations.get("kars.azure.com/sandbox-uid") == target["uid"]
            and annotations.get("kars.azure.com/sandbox-namespace") == CORE
            and annotations.get("kars.azure.com/sandbox-name") == target["sandbox"]
            and expected_uids and all(value == identity(namespace)[0] for value in expected_uids), UNAVAILABLE)
    secret_path = core(name, "secrets", "router-services-observer")
    policy_path = resource(name, "ciliumnetworkpolicies", group="/apis/cilium.io/v2") + "?" + urlencode({
        "labelSelector": f"{LABEL}={identity(grant)[0]}", "limit": 33})
    secret = metadata_read(reader, secret_path)
    policies = metadata_read(reader, policy_path, listed=True)
    secret_fact = {"status": "absent"}
    if secret is not None:
        secret_fact = {"status": "present", **bound_metadata(secret, namespace)}
        require(secret_fact["name"] == "router-services-observer", UNAVAILABLE)
        secret_fact["matchesPublishedVersion"] = (
            f"{secret_fact['uid']}:{secret_fact['resourceVersion']}" == observed["version"]
            if isinstance(observed.get("version"), str) else None)
    facts = []
    for policy in policies:
        fact = bound_metadata(policy, namespace)
        labels, annotations = policy["metadata"].get("labels", {}), policy["metadata"].get("annotations", {})
        require(labels.get(LABEL) == identity(grant)[0] and annotations.get(OWNER) == identity(grant)[0]
                and annotations.get("kars.azure.com/observer-namespace-uid") == identity(namespace)[0]
                and annotations.get("kars.azure.com/sandbox-uid") == target["uid"], UNAVAILABLE)
        generation = annotations.get("kars.azure.com/observer-grant-generation")
        require(isinstance(generation, str) and re.fullmatch(r"[1-9][0-9]{0,9}", generation), UNAVAILABLE)
        facts.append({**fact, "grantGeneration": integer(int(generation), 1)})
    require(len({item["uid"] for item in facts}) == len(facts), UNAVAILABLE)
    require(secret == metadata_read(reader, secret_path)
            and policies == metadata_read(reader, policy_path, listed=True), UNAVAILABLE)
    recheck_actor(SimpleNamespace(admin=reader), {"anchors": {
        source_path: source, f"/api/v1/namespaces/{name}": namespace,
        resource(CORE, "karscredentialgrants", "workspace"): grant}})
    return {"available": True, "namespace": metadata(namespace), "secret": secret_fact, "policies": facts}


def collect(setup, target, failed, since):
    result = {"diagnosticOnly": True, "originalResult": "failed", "category": "not-eligible",
              "controller": {"available": False}, "grant": {"available": False},
              "observerMetadata": {"available": False}, "reconcileStage": "unavailable"}
    if not isinstance(failed, dict) or failed.get("result") != "failed" or failed.get("failure") != FAILURE:
        return result
    reader = _Reader(setup.admin)
    scoped = SimpleNamespace(admin=reader)
    result["category"] = "failure-time-snapshot"
    try:
        actor = resolve_actor(scoped, "controller", target)
        require(len(actor["pods"]) <= 2, UNAVAILABLE)
        pods = [controller_pod(actor["anchors"][core(CORE, "pods", pod["name"])]) for pod in actor["pods"]]
        recheck_actor(scoped, actor)
        result["controller"] = {"available": True, "pods": pods, "warnings": {
            "status": "unavailable", "records": [], "reconcileStage": "unavailable"}}
    except ERRORS:
        pass
    try:
        grant = reader.get(resource(CORE, "karscredentialgrants", "workspace"))
        require(metadata(grant)["name"] == "workspace" and grant["metadata"].get("namespace") == CORE, UNAVAILABLE)
        current, observed = integer(grant["metadata"]["generation"], 1), integer(grant["status"]["observedGeneration"])
        result["grant"] = {"available": True, "identity": metadata(grant), "currentGeneration": current,
                           "observedGeneration": observed, "generationCurrent": current == observed}
        require(isinstance(target, dict) and target.get("workspace") == CORE, UNAVAILABLE)
        result["observerMetadata"] = observer_metadata(reader, target, grant)
    except ERRORS:
        pass
    if result["controller"]["available"]:
        try:
            reader.remaining()
            records = warnings(setup, reader, actor["pods"][0], since)
            recheck_actor(scoped, actor)
            result["controller"]["warnings"] = records
        except ERRORS:
            pass
    result["readCalls"] = reader.calls
    return result
