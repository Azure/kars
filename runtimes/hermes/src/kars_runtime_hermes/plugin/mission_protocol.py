# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded wire compatibility with @kars/mesh mission-{protocol,phase,inputs}."""

from __future__ import annotations

import hashlib
import json
import re

MAX_WIRE = 192 * 1024
MAX_INFERENCE_CALLS = 8
ID = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9._:-]{0,252}\Z")
DID = re.compile(r"did:mesh:[a-f0-9]{32}\Z")
SHA = re.compile(r"sha256:[a-f0-9]{64}\Z")
NAME = re.compile(r"[a-zA-Z0-9][a-zA-Z0-9._-]{0,127}\Z")
TARGET = ("taskName", "taskUid", "sandboxUid", "podUid", "agentDid", "dispatcherDid")
ACTIVITY = "router-observations-v1"


def require(condition, message="Invalid mission contract"):
    if not condition:
        raise ValueError(message)


def encoded(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")


def digest(value):
    return "sha256:" + hashlib.sha256(encoded(value)).hexdigest()


def text_hash(value):
    return "sha256:" + hashlib.sha256(value.encode("utf-8")).hexdigest()


def integer(value, minimum=0, maximum=2**53 - 1):
    return type(value) is int and minimum <= value <= maximum


def matches(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def keys(value, allowed, required=()):
    require(type(value) is dict and set(value) <= set(allowed) and set(required) <= set(value))


def loads(raw, limit=MAX_WIRE):
    require(isinstance(raw, (bytes, str)) and len(raw.encode() if isinstance(raw, str) else raw) <= limit)

    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, "Duplicate mission field")
            result[key] = value
        return result

    value = json.loads(raw, object_pairs_hook=pairs, parse_constant=lambda _: require(False))
    nodes = 0

    def visit(item, depth):
        nonlocal nodes
        nodes += 1
        require(nodes <= 16384 and depth <= 16)
        if isinstance(item, dict):
            for key, child in item.items():
                visit(key, depth + 1)
                visit(child, depth + 1)
        elif isinstance(item, list):
            for child in item:
                visit(child, depth + 1)
    visit(value, 0)
    require(len(encoded(value)) <= limit)
    return value


def input_name(value):
    return matches(NAME, value) and value not in {"__proto__", "constructor", "prototype"}


def artifact_name(value):
    return input_name(value) and value != "response.md"


def artifacts(value):
    require(type(value) is dict and len(value) <= 16 and len(encoded(value)) <= 128 * 1024)
    require(all(artifact_name(k) and isinstance(v, str) and v.strip() for k, v in value.items()))
    return value


def phase(value):
    keys(value, ("name", "objective", "capabilities", "requiredToolCalls", "minToolCalls", "maxToolCalls",
                 "freshContext"), ("name", "objective", "maxToolCalls"))
    caps = value.get("capabilities", [])
    low, high = value.get("minToolCalls", 0), value["maxToolCalls"]
    require(matches(re.compile(r"[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?\Z"), value["name"]))
    objective = value["objective"]
    require(isinstance(objective, str) and 20 <= len(objective.strip().encode()) and len(objective.encode()) <= 1200)
    require(type(caps) is list and len(caps) <= 2 and all(c in ("filesystem-read", "filesystem-write") for c in caps)
            and len(set(caps)) == len(caps))
    require(value.get("requiredToolCalls", []) == [] and integer(low, 0, 32) and integer(high, low, 32))
    fresh = value.get("freshContext", False)
    require(type(fresh) is bool and (low == 0 or bool(caps)))
    return {"name": value["name"], "objective": objective, "capabilities": sorted(caps), "requiredToolCalls": [],
            "minToolCalls": low, "maxToolCalls": high, "freshContext": fresh}


def contract(value):
    require(type(value) is dict and type(value.get("version")) is int)
    version = value["version"]
    if version == 1:
        keys(value, ("version",))
        return {"version": 1}
    require(version in (2, 3, 4))
    has_inputs = version == 3 or (version == 4 and "inputDigest" in value)
    fields = ["version", "reviewedPhase", "phaseDigest"]
    if has_inputs:
        fields.append("inputDigest")
    if version == 4:
        fields.append("requiredOutputs")
    keys(value, fields, fields)
    reviewed = phase(value["reviewedPhase"])
    require(value["phaseDigest"] == digest(reviewed))
    result = {"version": version, "reviewedPhase": reviewed, "phaseDigest": value["phaseDigest"]}
    if has_inputs:
        require(matches(SHA, value["inputDigest"]) and "filesystem-read" in reviewed["capabilities"]
                and reviewed["maxToolCalls"] > 0)
        result["inputDigest"] = value["inputDigest"]
    if version == 4:
        names = value["requiredOutputs"]
        require(type(names) is list and len(names) <= 16 and all(artifact_name(n) for n in names)
                and len(set(names)) == len(names))
        require(not names or ("filesystem-write" in reviewed["capabilities"] and reviewed["maxToolCalls"] > 0),
                "Required exports need a write-capable phase")
        result["requiredOutputs"] = sorted(names)
    return result


def inputs(values):
    require(type(values) is list and 1 <= len(values) <= 16)
    result = []
    source_fields = ("namespace", "taskName", "taskUid", "sandboxUid", "podUid", "runNonce", "assignmentId",
                     "agentDid", "artifactName")
    for value in values:
        keys(value, ("name", "source", "sha256", "content"), ("name", "source", "sha256", "content"))
        source, content = value["source"], value["content"]
        keys(source, source_fields, source_fields)
        require(input_name(value["name"]) and input_name(source["artifactName"]))
        require(matches(re.compile(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\Z"), source["namespace"]))
        require(all(matches(ID, source[k]) for k in source_fields[1:7]) and matches(DID, source["agentDid"]))
        require(isinstance(content, str) and content.strip() and len(content.encode()) <= 65536)
        require(value["sha256"] == text_hash(content))
        result.append({"name": value["name"], "source": {k: source[k] for k in source_fields},
                       "sha256": value["sha256"], "content": content})
    require(len({v["name"] for v in result}) == len(result))
    result.sort(key=lambda v: v["name"])
    require(len(encoded(result)) <= 65536)
    return result


def admission(value):
    common = ("version", "taskGeneration", "authorizationDigest", "state")
    require(type(value) is dict and value.get("state") in ("idle", "run"))
    fields = common + (("runNonce", "objectiveDigest") if value["state"] == "run" else ())
    keys(value, fields, fields)
    require(type(value["version"]) is int and value["version"] == 1 and integer(value["taskGeneration"], 1)
            and matches(SHA, value["authorizationDigest"]))
    if value["state"] == "run":
        require(matches(ID, value["runNonce"]) and matches(SHA, value["objectiveDigest"]))
    return value


def message(raw, expected, target, admitted):
    value = loads(raw)
    require(type(value) is dict and value.get("type") in ("mission:probe", "mission:assign"))
    assignment = value["type"] == "mission:assign"
    fields = set(TARGET) | set(expected) | {"type", "runNonce", "activityFormat"}
    fields |= {"bootId", "assignmentId", "artifactFormat", "content", "inputArtifacts"} if assignment else {"challenge"}
    keys(value, fields, set(TARGET) | set(expected) | {"type", "runNonce"})
    require(all(value[k] == v for k, v in target.items()) and matches(ID, value["runNonce"]))
    require(contract({k: value[k] for k in expected}) == expected)
    require("activityFormat" not in value or value["activityFormat"] == ACTIVITY)
    require(admitted["state"] == "run" and value["runNonce"] == admitted["runNonce"])
    if assignment:
        require(matches(ID, value.get("bootId")) and matches(ID, value.get("assignmentId")))
        content = value.get("content")
        require(isinstance(content, str) and content.strip() and text_hash(content) == admitted["objectiveDigest"])
        require("artifactFormat" not in value or value["artifactFormat"] == "text-v1")
        require(not expected.get("requiredOutputs") or value.get("artifactFormat") == "text-v1")
        if "inputDigest" in expected:
            value["inputArtifacts"] = inputs(value.get("inputArtifacts"))
            require(digest({"schema": "kars.mission-inputs/v1", "inputs": value["inputArtifacts"]})
                    == expected["inputDigest"])
        else:
            require("inputArtifacts" not in value)
    else:
        require(matches(ID, value.get("challenge")))
    return value


def evidence(value, reviewed=None, previous=None, success=False):
    required = ("model", "rounds", "usage") + (("phase",) if reviewed else ())
    keys(value, required, required)
    require(isinstance(value["model"], str) and value["model"].strip()
            and integer(value["rounds"], 0, MAX_INFERENCE_CALLS))
    usage = value["usage"]
    if usage is not None:
        fields = ("promptTokens", "completionTokens", "totalTokens")
        keys(usage, fields, fields)
        require(all(integer(usage[k]) for k in fields)
                and usage["totalTokens"] == usage["promptTokens"] + usage["completionTokens"])
    if reviewed:
        p = value["phase"]
        fields = ("name", "attemptedToolCalls", "successfulToolCalls", "minToolCalls", "maxToolCalls")
        keys(p, fields, fields)
        require(all(p[k] == reviewed[k] for k in ("name", "minToolCalls", "maxToolCalls")))
        require(integer(p["attemptedToolCalls"], 0, reviewed["maxToolCalls"])
                and integer(p["successfulToolCalls"], 0, p["attemptedToolCalls"]))
        require(not success or p["successfulToolCalls"] >= reviewed["minToolCalls"])
    if previous:
        require(value["model"] == previous["model"] and value["rounds"] >= previous["rounds"])
        require(not (previous["rounds"] > 0 and previous["usage"] is None and usage is not None))
        if usage and previous["usage"]:
            require(all(usage[k] >= previous["usage"][k] for k in usage))
        if reviewed:
            require(all(value["phase"][k] >= previous["phase"][k]
                        for k in ("attemptedToolCalls", "successfulToolCalls")))
    require(not success or (value["rounds"] > 0 and usage is not None and usage["totalTokens"] > 0))
    return loads(encoded(value))
