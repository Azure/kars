# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Bounded wire compatibility with @kars/mesh mission-{protocol,phase,inputs}."""

from __future__ import annotations

import hashlib
import json
import re

MAX_BASE = 192 * 1024
MAX_ACTIVITY = 384 * 1024
MAX_WIRE = MAX_BASE + MAX_ACTIVITY
MAX_INPUT = MAX_WIRE - MAX_BASE
# Eight final-phase producers, eight memory files, and sixteen revision files.
MAX_INPUT_FILES = 8 * 16 + 8 + 16
MAX_OBSERVATION_ROUNDS = 130
MAX_OBSERVATION_EVENTS = 1024
MAX_OBSERVATION_PAGE = 256 * 1024
OBSERVATION_ARTIFACT = "kars-router-observations.json"
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


def message_fits(value):
    base = {k: v for k, v in value.items() if k != "routerObservations"}
    if value.get("type") == "mission:assign" and "inputArtifacts" in base:
        if len(encoded(base.pop("inputArtifacts"))) > MAX_INPUT:
            return False
    if isinstance(base.get("artifacts"), dict):
        base["artifacts"] = {k: v for k, v in base["artifacts"].items() if k != OBSERVATION_ARTIFACT}
    return len(encoded(base)) <= MAX_BASE and len(encoded(value)) <= MAX_WIRE


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


def loads(raw, limit=MAX_BASE, *, max_nodes=16384):
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
        require(nodes <= max_nodes and depth <= 16)
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
    require(type(value) is dict and len(value) <= 16)
    require(all(artifact_name(k) and isinstance(v, str) and v.strip() for k, v in value.items()))
    require(len(encoded({k: v for k, v in value.items() if k != OBSERVATION_ARTIFACT})) <= 128 * 1024)
    require(
        OBSERVATION_ARTIFACT not in value
        or len(encoded({OBSERVATION_ARTIFACT: value[OBSERVATION_ARTIFACT]})) <= MAX_ACTIVITY
    )
    return value


def reviewed_fetch_url(value, *, allow_origin=False):
    if not isinstance(value, str) or len(value) > 2048 or not value.startswith("https://"):
        return False
    host, slash, path = value[8:].partition("/")
    labels = host.split(".")
    return (
        (bool(slash) or allow_origin)
        and len(host) <= 253
        and len(labels) >= 2
        and all(re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label) for label in labels)
        and re.fullmatch(r"[a-z]{2,63}", labels[-1]) is not None
        and labels[-1] not in ("local", "internal", "localhost")
        and re.fullmatch(r"[A-Za-z0-9/_~.!$&'()*+,;=:@?%-]*", path) is not None
        and re.search(r"%(?![0-9a-fA-F]{2})|%2e", path, re.I) is None
        and not any(part in (".", "..") for part in path.split("?")[0].split("/"))
    )


def reviewed_fetch_arguments(reviewed, args):
    return (
        type(args) is dict
        and {"method", "url"} <= set(args)
        and set(args)
        <= (
            {"method", "url", "wait_for_approval_ms"}
            if reviewed.get("executionMode") == "native"
            else {"method", "url"}
        )
        and (
            "wait_for_approval_ms" not in args
            or (type(args["wait_for_approval_ms"]) is int and 0 <= args["wait_for_approval_ms"] <= 300000)
        )
        and args["method"] == "GET"
        and "network" in reviewed["capabilities"]
        and (
            reviewed_fetch_url(args["url"], allow_origin=True)
            if reviewed.get("executionMode") == "native"
            else any(call["arguments"] == args for call in reviewed["requiredToolCalls"])
        )
    )


def phase(value):
    keys(
        value,
        (
            "name",
            "objective",
            "capabilities",
            "requiredToolCalls",
            "minToolCalls",
            "maxToolCalls",
            "freshContext",
            "executionMode",
        ),
        ("name", "objective", "maxToolCalls"),
    )
    require("executionMode" not in value or value["executionMode"] == "native", "Unsupported phase execution mode")
    caps = value.get("capabilities", [])
    low, high = value.get("minToolCalls", 0), value["maxToolCalls"]
    require(matches(re.compile(r"[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?\Z"), value["name"]))
    objective = value["objective"]
    require(isinstance(objective, str) and 20 <= len(objective.strip().encode()) and len(objective.encode()) <= 1200)
    supported = ("filesystem-read", "filesystem-write", "network")
    if value.get("executionMode") == "native":
        supported += ("memory", "github-pr", "python-execute")
    require(
        type(caps) is list
        and len(caps) <= len(supported)
        and all(c in supported for c in caps)
        and len(set(caps)) == len(caps),
        "Only filesystem and network are supported; scoped memory, github-pr and "
        "python-execute require native execution",
    )
    require(
        "python-execute" not in caps or {"filesystem-read", "filesystem-write", "network"} <= set(caps),
        "Native python-execute requires explicit filesystem-read, filesystem-write and network capabilities",
    )
    maximum = 2_147_483_647 if value.get("executionMode") == "native" else 32
    require(integer(low, 0, maximum) and integer(high, low, maximum))
    required = value.get("requiredToolCalls", [])
    require(type(required) is list and len(required) <= min(8, high))
    calls = []
    for call in required:
        keys(call, ("name", "arguments"), ("name", "arguments"))
        args = call["arguments"]
        keys(args, ("method", "url"), ("method", "url"))
        require(
            call["name"] == "http_fetch" and args["method"] == "GET" and reviewed_fetch_url(args["url"]),
            "Required calls support only exact HTTPS http_fetch GET; hosted web-search "
            "is outside the finite funding contract",
        )
        calls.append({"name": "http_fetch", "arguments": {"method": "GET", "url": args["url"]}})
    require(
        (not calls or "network" in caps)
        and (value.get("executionMode") == "native" or ("network" in caps) == bool(calls))
        and len({c["arguments"]["url"] for c in calls}) == len(calls),
        "Network requires unique reviewed http_fetch URLs and network capability",
    )
    fresh = value.get("freshContext", False)
    require(type(fresh) is bool and (low == 0 or bool(caps)))
    return {
        "name": value["name"],
        "objective": objective,
        "capabilities": sorted(caps),
        "requiredToolCalls": calls,
        "minToolCalls": low,
        "maxToolCalls": high,
        "freshContext": fresh,
        **({"executionMode": "native"} if value.get("executionMode") == "native" else {}),
    }


def peer_context(value):
    fields = ("rootUid", "stateName", "slot", "peers")
    keys(value, fields, fields)
    require(
        matches(ID, value["rootUid"])
        and matches(re.compile(r"kars-reviewed-[a-f0-9]{40}\Z"), value["stateName"])
        and matches(re.compile(r"(?:r[0-7]-p[0-7]|synthesis)\Z"), value["slot"])
    )
    peers = value["peers"]
    require(type(peers) is list and len(peers) <= 64)
    name = re.compile(r"[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?\Z")
    slots, phases, roles, indices = set(), set(), {}, {}
    result = []
    for peer in peers:
        fields = ("role", "phase", "slot", "agentName")
        keys(peer, fields, fields)
        require(
            matches(name, peer["role"])
            and matches(name, peer["phase"])
            and matches(re.compile(r"r[0-7]-p[0-7]\Z"), peer["slot"])
        )
        index = peer["slot"][:2]
        require(
            peer["agentName"] == "rp-" + value["stateName"][14:34] + "-" + peer["slot"]
            and peer["slot"] not in slots
            and (peer["role"], peer["phase"]) not in phases
            and roles.get(peer["role"], index) == index
            and indices.get(index, peer["role"]) == peer["role"]
        )
        slots.add(peer["slot"])
        phases.add((peer["role"], peer["phase"]))
        roles[peer["role"]], indices[index] = index, peer["role"]
        result.append({key: peer[key] for key in fields})
    require(value["slot"] == "synthesis" or value["slot"] in slots)
    return {
        "rootUid": value["rootUid"],
        "stateName": value["stateName"],
        "slot": value["slot"],
        "peers": sorted(result, key=lambda peer: peer["slot"]),
    }


def contract(value):
    require(type(value) is dict and type(value.get("version")) is int)
    version = value["version"]
    if version == 1:
        keys(value, ("version",))
        return {"version": 1}
    require(version in (2, 3, 4, 5))
    has_inputs = version == 3 or (version in (4, 5) and "inputDigest" in value)
    fields = ["version", "reviewedPhase", "phaseDigest"]
    if has_inputs:
        fields.append("inputDigest")
    if version in (4, 5):
        fields.append("requiredOutputs")
    if version == 5:
        fields.append("peerContext")
    keys(value, fields, fields)
    reviewed = phase(value["reviewedPhase"])
    require(value["phaseDigest"] == digest(reviewed))
    result = {"version": version, "reviewedPhase": reviewed, "phaseDigest": value["phaseDigest"]}
    if has_inputs:
        require(
            matches(SHA, value["inputDigest"])
            and "filesystem-read" in reviewed["capabilities"]
            and reviewed["maxToolCalls"] > 0
        )
        result["inputDigest"] = value["inputDigest"]
    if version in (4, 5):
        names = value["requiredOutputs"]
        require(
            type(names) is list
            and len(names) <= 16
            and all(artifact_name(n) for n in names)
            and len(set(names)) == len(names)
        )
        require(
            not names or ("filesystem-write" in reviewed["capabilities"] and reviewed["maxToolCalls"] > 0),
            "Required exports need a write-capable phase",
        )
        result["requiredOutputs"] = sorted(names)
    if version == 5:
        result["peerContext"] = peer_context(value["peerContext"])
    return result


def inputs(values):
    require(type(values) is list and 1 <= len(values) <= MAX_INPUT_FILES)
    result = []
    source_fields = (
        "namespace",
        "taskName",
        "taskUid",
        "sandboxUid",
        "podUid",
        "runNonce",
        "assignmentId",
        "agentDid",
        "artifactName",
    )
    for value in values:
        keys(value, ("name", "source", "sha256", "content"), ("name", "source", "sha256", "content"))
        source, content = value["source"], value["content"]
        keys(source, source_fields, source_fields)
        require(input_name(value["name"]) and input_name(source["artifactName"]))
        require(matches(re.compile(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\Z"), source["namespace"]))
        require(all(matches(ID, source[k]) for k in source_fields[1:7]) and matches(DID, source["agentDid"]))
        require(isinstance(content, str) and content.strip() and len(content.encode()) <= 65536)
        require(value["sha256"] == text_hash(content))
        result.append(
            {
                "name": value["name"],
                "source": {k: source[k] for k in source_fields},
                "sha256": value["sha256"],
                "content": content,
            }
        )
    require(len({v["name"] for v in result}) == len(result))
    result.sort(key=lambda v: v["name"])
    require(len(encoded(result)) <= MAX_INPUT)
    return result


def configured_contract(raw):
    value = loads(raw, 32768)
    result = contract(value)
    require(result["version"] == 5 or len(raw.encode("utf-8")) <= 8192)
    return result


def admission(value):
    common = ("version", "taskGeneration", "authorizationDigest", "state")
    require(type(value) is dict and value.get("state") in ("idle", "run"))
    fields = common + (("runNonce", "objectiveDigest") if value["state"] == "run" else ())
    keys(value, fields, fields)
    require(
        type(value["version"]) is int
        and value["version"] == 1
        and integer(value["taskGeneration"], 1)
        and matches(SHA, value["authorizationDigest"])
    )
    if value["state"] == "run":
        require(matches(ID, value["runNonce"]) and matches(SHA, value["objectiveDigest"]))
    return value


def message(raw, expected, target, admitted):
    value = loads(raw, MAX_WIRE)
    require(type(value) is dict and value.get("type") in ("mission:probe", "mission:assign"))
    require(message_fits(value), "Mission message exceeds byte limits")
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
            require(
                digest({"schema": "kars.mission-inputs/v1", "inputs": value["inputArtifacts"]})
                == expected["inputDigest"]
            )
        else:
            require("inputArtifacts" not in value)
    else:
        require(matches(ID, value.get("challenge")))
    return value


def inference_call_limit(reviewed=None):
    if reviewed and reviewed.get("executionMode") == "native":
        # Allow a model round per reviewed tool attempt plus answer/finalization.
        return max(MAX_INFERENCE_CALLS, reviewed["maxToolCalls"] + 2)
    return MAX_INFERENCE_CALLS


def native_timeout_seconds(reviewed=None):
    if reviewed and reviewed.get("executionMode") == "native":
        return min(2_147_483_647, inference_call_limit(reviewed) * 630_000 + reviewed["maxToolCalls"] * 300_000) / 1000
    return 600


def native_event_limit(reviewed=None):
    if reviewed and reviewed.get("executionMode") == "native":
        return max(128, 2 * inference_call_limit(reviewed) + 3 * reviewed["maxToolCalls"] + 2)
    return 128


def evidence(value, reviewed=None, previous=None, success=False):
    required = ("model", "rounds", "usage") + (("phase",) if reviewed else ())
    keys(value, required, required)
    require(
        isinstance(value["model"], str)
        and value["model"].strip()
        and integer(value["rounds"], 0, inference_call_limit(reviewed))
    )
    usage = value["usage"]
    if usage is not None:
        fields = ("promptTokens", "completionTokens", "totalTokens")
        keys(usage, fields, fields)
        require(
            all(integer(usage[k]) for k in fields)
            and usage["totalTokens"] == usage["promptTokens"] + usage["completionTokens"]
        )
    if reviewed:
        p = value["phase"]
        fields = ("name", "attemptedToolCalls", "successfulToolCalls", "minToolCalls", "maxToolCalls")
        keys(p, (*fields, "completedRequiredToolCalls"), fields)
        require(
            integer(p["attemptedToolCalls"], 0, reviewed["maxToolCalls"])
            and integer(p["successfulToolCalls"], 0, p["attemptedToolCalls"])
        )
        completed = p.get("completedRequiredToolCalls", 0)
        require(
            integer(completed, 0, len(reviewed["requiredToolCalls"]))
            and completed <= p["successfulToolCalls"]
            and (not success or completed == len(reviewed["requiredToolCalls"])),
            "Required reviewed fetch calls were not successfully completed",
        )
        require(all(p[k] == reviewed[k] for k in ("name", "minToolCalls", "maxToolCalls")))
        require(not success or p["successfulToolCalls"] >= reviewed["minToolCalls"])
    if previous:
        require(value["model"] == previous["model"] and value["rounds"] >= previous["rounds"])
        require(not (previous["rounds"] > 0 and previous["usage"] is None and usage is not None))
        if usage and previous["usage"]:
            require(all(usage[k] >= previous["usage"][k] for k in usage))
        if reviewed:
            require(
                all(value["phase"][k] >= previous["phase"][k] for k in ("attemptedToolCalls", "successfulToolCalls"))
            )
            require(
                value["phase"].get("completedRequiredToolCalls", 0)
                >= previous["phase"].get("completedRequiredToolCalls", 0)
            )
    require(not success or (value["rounds"] > 0 and usage is not None and usage["totalTokens"] > 0))
    return loads(encoded(value))
