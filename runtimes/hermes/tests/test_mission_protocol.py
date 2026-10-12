# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from pathlib import Path
import subprocess

import pytest

from kars_runtime_hermes.plugin import mission_protocol as wire


def binding():
    target = dict(taskName="mission", taskUid="task-uid", sandboxUid="sandbox-uid", podUid="pod-uid",
                  agentDid="did:mesh:" + "a" * 32, dispatcherDid="did:mesh:" + "b" * 32)
    phase = wire.phase(dict(name="draft", objective="Read the immutable evidence and produce a report",
                           capabilities=["filesystem-write", "filesystem-read"], minToolCalls=1, maxToolCalls=4))
    source = dict(namespace="kars-source", taskName="source", taskUid="source-uid", sandboxUid="source-sandbox",
                  podUid="source-pod", runNonce="source-run", assignmentId="source-assignment",
                  agentDid="did:mesh:" + "c" * 32, artifactName="response.md")
    inputs = wire.inputs([dict(name="facts.txt", source=source, content="Evidence: café ☕",
                               sha256=wire.text_hash("Evidence: café ☕"))])
    contract = wire.contract(dict(version=4, reviewedPhase=phase, phaseDigest=wire.digest(phase),
                                 inputDigest=wire.digest(dict(schema="kars.mission-inputs/v1", inputs=inputs)),
                                 requiredOutputs=["report.md"]))
    content = "Create the named report from the immutable input"
    admitted = dict(version=1, state="run", taskGeneration=7, authorizationDigest="sha256:" + "e" * 64,
                    runNonce="nonce-1", objectiveDigest=wire.text_hash(content))
    assignment = dict(**target, **contract, type="mission:assign", runNonce="nonce-1", bootId="boot",
                      assignmentId="assignment", artifactFormat="text-v1", content=content, inputArtifacts=inputs)
    return target, contract, admitted, assignment


def test_canonical_phase_and_inputs_match_actual_typescript_modules():
    _, contract, _, assignment = binding()
    repo = Path(__file__).resolve().parents[3]
    script = f"""
        import {{missionPhaseDigest}} from {json.dumps((repo / 'mesh-plugin/src/mission-phase.ts').as_uri())};
        import {{missionInputsDigest}} from {json.dumps((repo / 'mesh-plugin/src/mission-inputs.ts').as_uri())};
        const c = {json.dumps(contract)}; const inputs = {json.dumps(assignment['inputArtifacts'])};
        console.log(JSON.stringify([missionPhaseDigest(c.reviewedPhase), missionInputsDigest(inputs)]));
    """
    result = subprocess.run(["node", "--experimental-strip-types", "--input-type=module", "-e", script],
                            capture_output=True, text=True, check=True)
    assert json.loads(result.stdout) == [contract["phaseDigest"], contract["inputDigest"]]


@pytest.mark.parametrize("raw", [b'{"type":"mission:assign","type":"file_transfer"}', b'{"a":NaN}',
                                  b'{"a":Infinity}', b'{"a":"\xff"}', b'[' * 18 + b'0' + b']' * 18,
                                  b'"' + b'a' * wire.MAX_WIRE + b'"', b'{"a":"\\ud800"}'])
def test_wire_rejects_ambiguous_unbounded_or_non_utf8_json(raw):
    with pytest.raises((ValueError, UnicodeError)):
        wire.loads(raw)


@pytest.mark.parametrize("field,value", [("taskUid", "wrong"), ("sandboxUid", "wrong"), ("podUid", "wrong"),
                                         ("agentDid", "did:mesh:" + "f" * 32), ("runNonce", "wrong"),
                                         ("content", "changed"), ("extra", True), ("artifactFormat", "binary"),
                                         ("activityFormat", "unsupported"), ("assignmentId", "")])
def test_assignment_binding_rejects_mismatch(field, value):
    target, contract, admitted, assignment = binding()
    assignment[field] = value
    with pytest.raises(ValueError):
        wire.message(wire.encoded(assignment), contract, target, admitted)


def test_immutable_input_content_and_provenance_are_bound():
    target, contract, admitted, assignment = binding()
    assert wire.message(wire.encoded(assignment), contract, target, admitted) == assignment
    for field in ("content", "source"):
        changed = copy.deepcopy(assignment)
        if field == "content":
            changed["inputArtifacts"][0][field] = "substituted"
        else:
            changed["inputArtifacts"][0][field]["taskUid"] = "substituted"
        with pytest.raises(ValueError):
            wire.message(wire.encoded(changed), contract, target, admitted)


@pytest.mark.parametrize("version", [1, 2, 3, 4])
def test_supported_contract_versions(version):
    _, contract, _, _ = binding()
    contract["version"] = version
    if version < 4:
        del contract["requiredOutputs"]
    if version < 3:
        del contract["inputDigest"]
    if version < 2:
        del contract["reviewedPhase"], contract["phaseDigest"]
    assert wire.contract(contract) == contract


@pytest.mark.parametrize("change", [dict(capabilities=["network"]), dict(requiredToolCalls=["terminal"]),
                                    dict(maxToolCalls=33), dict(maxToolCalls=True), dict(freshContext=1)])
def test_unsupported_phase_fails_closed(change):
    _, contract, _, _ = binding()
    with pytest.raises(ValueError):
        wire.phase({**contract["reviewedPhase"], **change})


def test_required_exports_need_write_and_artifact_negotiation():
    target, contract, admitted, assignment = binding()
    del assignment["artifactFormat"]
    with pytest.raises(ValueError):
        wire.message(wire.encoded(assignment), contract, target, admitted)
    contract["reviewedPhase"]["capabilities"] = ["filesystem-read"]
    contract["phaseDigest"] = wire.digest(contract["reviewedPhase"])
    with pytest.raises(ValueError, match="write-capable"):
        wire.contract(contract)


def test_unknown_usage_never_becomes_known_after_an_attempt():
    known = dict(model="model", rounds=1, usage=dict(promptTokens=10, completionTokens=5, totalTokens=15))
    with pytest.raises(ValueError):
        wire.evidence(known, previous={**known, "usage": None})
    with pytest.raises(ValueError):
        wire.evidence({**known, "usage": None}, success=True)
    assert wire.evidence({**known, "usage": None}, previous=known)["usage"] is None


@pytest.mark.parametrize("name", ["../escape", "__proto__", "constructor", "prototype", "/absolute"])
def test_unsafe_input_names_rejected(name):
    _, _, _, assignment = binding()
    assignment["inputArtifacts"][0]["name"] = name
    with pytest.raises(ValueError):
        wire.inputs(assignment["inputArtifacts"])


def test_response_is_valid_input_but_not_named_export():
    assert wire.input_name("response.md")
    assert not wire.artifact_name("response.md")
