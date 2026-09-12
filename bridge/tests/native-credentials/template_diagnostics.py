"""Failure-only template comparisons; neither template values nor hashes are published."""

import json
import re

from native_api import ROOT, Failure, command, require, resource
from observation_diagnostics import READ_ERRORS

ACTORS = ("runtime", "controller", "bff")
LIMIT = 2 * 1024 * 1024
UNAVAILABLE = "Native template comparison unavailable"
HASH_SCRIPT = """
import {readFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const {templateDigest} = await import(pathToFileURL(process.argv[1]).href);
process.stdout.write(JSON.stringify(JSON.parse(readFileSync(0, 'utf8')).map(templateDigest)));
"""


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def identity(value):
    require(isinstance(value, dict) and value.get("kind") == "Deployment"
            and value.get("apiVersion") == "apps/v1", UNAVAILABLE)
    meta = value.get("metadata")
    require(isinstance(meta, dict) and not meta.get("deletionTimestamp")
            and all(isinstance(meta.get(key), str) and meta[key]
                    for key in ("name", "namespace", "uid", "resourceVersion")), UNAVAILABLE)
    return meta["namespace"], meta["name"], meta["uid"]


def template(value):
    require(isinstance(value.get("spec"), dict), UNAVAILABLE)
    result = value["spec"].get("template")
    require(isinstance(result, dict) and isinstance(result.get("metadata"), dict)
            and isinstance(result.get("spec"), dict), UNAVAILABLE)
    return result


def hashes(values):
    raw = encoded(values)
    require(len(raw.encode("utf8")) <= LIMIT, UNAVAILABLE)
    module = ROOT / ".native/core/cli/dist/lib/private-activation.js"
    result = command("node", "--input-type=module", "-e", HASH_SCRIPT, str(module),
                     stdin=raw, timeout=15)
    require(len(result) <= 512, UNAVAILABLE)
    parsed = json.loads(result)
    require(isinstance(parsed, list) and len(parsed) == len(values)
            and all(isinstance(value, str) and re.fullmatch(r"[a-f0-9]{64}", value)
                    for value in parsed), UNAVAILABLE)
    return parsed


def container_changes(before, current):
    require(all(isinstance(values, list) and len(values) <= 16
                and all(isinstance(value, dict) and isinstance(value.get("name"), str)
                        for value in values) for values in (before, current)), UNAVAILABLE)
    groups = {
        "imagesChanged": ("image", "imagePullPolicy"),
        "environmentChanged": ("env", "envFrom"),
        "commandsChanged": ("command", "args", "workingDir"),
        "mountsChanged": ("volumeMounts", "volumeDevices"),
        "containerSecurityChanged": ("securityContext",),
        "containerResourcesChanged": ("resources",),
    }
    result = {}
    for name, fields in groups.items():
        def select(values):
            return [{key: value[key] for key in ("name", *fields) if key in value} for value in values]
        result[name] = encoded(select(before)) != encoded(select(current))
    known = {"name", *(key for fields in groups.values() for key in fields)}
    result["otherContainerFieldsChanged"] = encoded([
        {key: value for key, value in entry.items() if key not in known} for entry in before
    ]) != encoded([{key: value for key, value in entry.items() if key not in known} for entry in current])
    return result


def project(before, current, review, before_digest, current_digest):
    previous_identity = identity(before)
    current_identity = identity(current)
    require(previous_identity[:2] == current_identity[:2], UNAVAILABLE)
    require(isinstance(review, dict) and review.get("kind") == "Deployment"
            and isinstance(review.get("object"), dict)
            and review["object"].get("name") == previous_identity[1]
            and review["object"].get("uid") == previous_identity[2]
            and isinstance(review["object"].get("resourceVersion"), str)
            and review["object"]["resourceVersion"]
            and isinstance(review.get("templateDigest"), str)
            and re.fullmatch(r"[a-f0-9]{64}", review["templateDigest"]), UNAVAILABLE)
    baseline, actual = template(before), template(current)
    # Hashes come from the shipped CLI, including its private-epoch normalization.
    same_uid = previous_identity[2] == current_identity[2]
    result = {
        "available": True,
        "sameUid": same_uid,
        "baselineMatchesReview": before_digest == review["templateDigest"],
        "currentMatchesReview": same_uid and current_digest == review["templateDigest"],
        "sameTemplate": before_digest == current_digest,
    }
    for key in ("labels", "annotations"):
        result[key + "Changed"] = encoded(baseline["metadata"].get(key)) != encoded(actual["metadata"].get(key))
    sections = ("containers", "initContainers", "volumes", "securityContext", "serviceAccountName")
    for key in sections:
        result[key + "Changed"] = encoded(baseline["spec"].get(key)) != encoded(actual["spec"].get(key))
    result["otherPodSpecChanged"] = encoded({
        key: value for key, value in baseline["spec"].items() if key not in sections
    }) != encoded({key: value for key, value in actual["spec"].items() if key not in sections})
    result.update(container_changes(baseline["spec"].get("containers"), actual["spec"].get("containers")))
    return result


def collect(setup, baselines, failure):
    result = {"diagnosticOnly": True, "available": False, "category": "not-eligible"}
    if not isinstance(failure, str) or not any(failure.startswith(f"Native operator apply failed: {category} ")
            for category in ("consumer-template-drift", "writer-runtime-transition")):
        return result
    result["category"] = "unavailable"
    try:
        require(isinstance(baselines, dict) and set(baselines) == set(ACTORS), UNAVAILABLE)
        namespaces = {identity(value)[0] for value in baselines.values()}
        # The fixture uses one existing review document, never a fresh approval.
        review_path = ROOT / ".native/grant-review-kars-system.json"
        with review_path.open("rb") as stream:
            raw = stream.read(LIMIT + 1)
        require(len(raw) <= LIMIT, UNAVAILABLE)
        document = json.loads(raw)
        require(isinstance(document, dict) and document.get("kind") == "KarsCredentialGrant"
                and document.get("apiVersion") == "kars.azure.com/v1alpha1"
                and isinstance(document.get("metadata"), dict)
                and document["metadata"].get("namespace") == "kars-system"
                and document["metadata"].get("name") == "workspace", UNAVAILABLE)
        review = document["spec"]["privateActivation"]
        scopes = review["namespaces"]
        require(isinstance(scopes, list) and len(scopes) <= 32
                and review.get("phase") == "reviewed", UNAVAILABLE)
        require(all(isinstance(scope, dict) and isinstance(scope.get("namespace"), dict)
                    and isinstance(scope.get("consumers"), list) and len(scope["consumers"]) <= 32
                    and all(isinstance(consumer, dict) and isinstance(consumer.get("object"), dict)
                            for consumer in scope["consumers"]) for scope in scopes), UNAVAILABLE)
        selected = [scope for scope in scopes if scope["namespace"]["name"] in namespaces]
        comparisons = {}
        for actor in ACTORS:
            before = baselines[actor]
            namespace, name, uid = identity(before)
            matches = [consumer for scope in selected if scope["namespace"]["name"] == namespace
                       for consumer in scope["consumers"]
                       if consumer.get("kind") == "Deployment" and consumer["object"].get("uid") == uid]
            require(len(matches) == 1, UNAVAILABLE)
            path = resource(namespace, "deployments", name, "/apis/apps/v1")
            current = setup.admin.get(path)
            before_hash, current_hash = hashes([before, current])
            rechecked = setup.admin.get(path)
            require(identity(rechecked) == identity(current)
                    and rechecked["metadata"]["resourceVersion"] == current["metadata"]["resourceVersion"],
                    UNAVAILABLE)
            comparisons[actor] = project(before, current, matches[0], before_hash, current_hash)
        result.update(available=True, category="compared", actors=comparisons)
    except READ_ERRORS:
        result["category"] = "provenance-or-comparison-unavailable"
    return result
