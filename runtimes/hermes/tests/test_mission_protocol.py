# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from pathlib import Path
import subprocess

import pytest

from kars_runtime_hermes.plugin import mission_protocol as wire


def test_native_origin_fetch_does_not_widen_historical_exact_url_grants():
    native = {"executionMode": "native", "capabilities": ["network"]}
    args = {"method": "GET", "url": "https://example.net", "wait_for_approval_ms": 0}
    assert wire.reviewed_fetch_arguments(native, args)
    assert not wire.reviewed_fetch_url(args["url"])
    legacy = {"capabilities": ["network"], "requiredToolCalls": [
        {"name": "http_fetch", "arguments": {"method": "GET", "url": "https://example.net/"}}]}
    assert not wire.reviewed_fetch_arguments(legacy, {"method": "GET", "url": args["url"]})
    for url in ("https://127.0.0.1", "https://example.local", "https://user@example.net",
                "https://example.net#fragment", "https://example.net?x=1"):
        assert not wire.reviewed_fetch_arguments(native, {**args, "url": url})
    assert not wire.reviewed_fetch_arguments({**native, "capabilities": []}, args)
    assert not wire.reviewed_fetch_arguments(native, {**args, "method": "POST"})
    assert not wire.reviewed_fetch_arguments(native, {**args, "headers": {}})


@pytest.mark.parametrize("size", [160 * 1024, 192 * 1024])
def test_native_peer_frames_keep_ordinary_bound_when_activity_grows(size):
    import os
    import threading
    from kars_runtime_hermes.plugin.mission_peer import read_frame

    read, write = os.pipe()
    payload = wire.encoded({"result": "x" * size}) + b"\n"

    def send():
        try:
            with os.fdopen(write, "wb") as output:
                output.write(payload)
        except BrokenPipeError:
            pass

    writer = threading.Thread(target=send)
    writer.start()
    try:
        with os.fdopen(read, "rb") as stream:
            if len(payload) > wire.MAX_BASE:
                with pytest.raises(ValueError, match="byte limit"):
                    read_frame(stream, 5)
            else:
                assert read_frame(stream, 5) == {"result": "x" * size}
    finally:
        writer.join(timeout=5)
    assert not writer.is_alive()


def test_native_initial_assignment_frame_preserves_independent_input_budget():
    import os
    import threading
    from kars_runtime_hermes.plugin.mission_peer import read_frame

    _, contract, _, assignment = binding()
    original = assignment["inputArtifacts"][0]
    documents = [dict(original, name=f"memory-{i}.md", content="x" * 63000,
                      sha256=wire.text_hash("x" * 63000)) for i in range(6)]
    request = dict(assignment, contract=contract, inputArtifacts=documents, content="x" * 8192)
    payload = wire.encoded(request) + b"\n"
    assert wire.MAX_BASE < len(payload) < wire.MAX_WIRE
    read, write = os.pipe()

    def send():
        with os.fdopen(write, "wb") as output:
            output.write(payload)

    writer = threading.Thread(target=send)
    writer.start()
    try:
        with os.fdopen(read, "rb") as stream:
            received = read_frame(stream, 5, limit=wire.MAX_WIRE)
            assert received == request
            assert wire.message_fits(received)
    finally:
        writer.join(timeout=5)
    assert not writer.is_alive()
    assert not wire.message_fits(dict(request, content="x" * wire.MAX_BASE))
    assert not wire.message_fits(dict(request, inputArtifacts=documents + [documents[0]]))
    assert not wire.message_fits(dict(request, type="mission:probe"))


def test_only_activity_reservation_expands_native_event_frame():
    large = "x" * (300 * 1024)
    event = {"kind": "progress", "evidence": {}, "routerObservations": large}
    with pytest.raises(ValueError):
        wire.loads(wire.encoded(event))
    assert wire.message_fits(wire.loads(wire.encoded(event), wire.MAX_WIRE))
    assert not wire.message_fits({"kind": "peer_request", "arguments": {"content": large}})
    assert wire.message_fits({"kind": "result", "artifacts": {"kars-router-observations.json": large}})


def fetch_phase():
    return wire.phase(dict(name="research", objective="Read reviewed public official evidence",
                           capabilities=["network"], minToolCalls=0, maxToolCalls=3,
                           requiredToolCalls=[dict(name="http_fetch", arguments=dict(
                               method="GET", url="https://www.example.com/pricing?q=public%20plans"))]))


@pytest.mark.parametrize("url", ["http://example.com/", "https://example.com", "https://EXAMPLE.com/",
    "https://example.com:443/", "https://user@example.com/", "https://127.0.0.1/", "https://[::1]/",
    "https://x.local/", "https://x.internal/", "https://example.com/#part", "https://example.com/a/../b",
    "https://example.com/%2e/b", "https://example.com/%no", "https://example.com/white space", "https://example.com/é"])
def test_reviewed_fetch_rejects_noncanonical_urls(url):
    phase = fetch_phase()
    phase["requiredToolCalls"][0]["arguments"]["url"] = url
    with pytest.raises(ValueError):
        wire.phase(phase)


@pytest.mark.parametrize("patch", [dict(capabilities=[]), dict(requiredToolCalls=[]), dict(maxToolCalls=0),
    dict(requiredToolCalls=[dict(name="web_search", arguments=dict(query="prices"))])])
def test_reviewed_fetch_rejects_incompatible_authority(patch):
    with pytest.raises(ValueError):
        wire.phase({**fetch_phase(), **patch})


def test_reviewed_fetch_arguments_and_verified_success_are_exact():
    phase = fetch_phase()
    args = phase["requiredToolCalls"][0]["arguments"]
    assert wire.reviewed_fetch_arguments(phase, args)
    for changed in ({"url": args["url"]}, {**args, "method": "POST"}, {**args, "headers": {}},
                    {**args, "url": "https://www.example.com/unreviewed"}):
        assert not wire.reviewed_fetch_arguments(phase, changed)
    duplicate = copy.deepcopy(phase)
    duplicate["requiredToolCalls"] *= 2
    with pytest.raises(ValueError):
        wire.phase(duplicate)
    evidence = dict(model="model", rounds=1, usage=dict(promptTokens=1, completionTokens=1, totalTokens=2),
                    phase=dict(name=phase["name"], minToolCalls=0, maxToolCalls=3,
                               attemptedToolCalls=2, successfulToolCalls=2))
    with pytest.raises(ValueError):
        wire.evidence(evidence, phase, success=True)
    evidence["phase"]["completedRequiredToolCalls"] = 1
    assert wire.evidence(evidence, phase, success=True) == evidence
    for count in (None, -1, 0.5, 2, "1", True):
        changed = copy.deepcopy(evidence)
        changed["phase"]["completedRequiredToolCalls"] = count
        with pytest.raises(ValueError):
            wire.evidence(changed, phase, success=True)
    changed = copy.deepcopy(evidence)
    changed["phase"]["completedRequiredToolCalls"] = 0
    with pytest.raises(ValueError):
        wire.evidence(changed, phase, previous=evidence)
    changed["phase"]["successfulToolCalls"] = None
    with pytest.raises(ValueError):
        wire.evidence(changed, phase)


@pytest.mark.parametrize("tool_count,limit", [(0, 8), (4, 8), (10, 12), (30, 32), (32, 34), (128, 130), (2_147_483_647, 2_147_483_649)])
def test_native_inference_limit_matches_reviewed_tools_and_evidence(tool_count, limit):
    phase = wire.phase(dict(name="review", objective="Review the supplied draft",
                            capabilities=["filesystem-read"], minToolCalls=0,
                            maxToolCalls=tool_count, executionMode="native"))
    assert wire.inference_call_limit(phase) == limit
    observed = dict(model="model", rounds=limit,
                    usage=dict(promptTokens=limit, completionTokens=limit, totalTokens=2 * limit),
                    phase=dict(name="review", minToolCalls=0, maxToolCalls=tool_count,
                               attemptedToolCalls=0, successfulToolCalls=0))
    assert wire.evidence(observed, phase, success=True) == observed
    with pytest.raises(ValueError):
        wire.evidence({**observed, "rounds": limit + 1}, phase)
    legacy = {k: v for k, v in phase.items() if k != "executionMode"}
    assert wire.inference_call_limit(legacy) == wire.inference_call_limit() == 8
    with pytest.raises(ValueError):
        wire.evidence({**observed, "rounds": 9}, legacy)


@pytest.mark.parametrize("maximum", [-1, 1.5, True, 2_147_483_648])
def test_native_headroom_rejects_invalid_wire_integers(maximum):
    with pytest.raises(ValueError):
        wire.phase({**fetch_phase(), "executionMode": "native", "maxToolCalls": maximum})


def test_native_deadlines_and_event_headroom_match_shared_dispatcher():
    phase = {**fetch_phase(), "executionMode": "native", "maxToolCalls": 128}
    assert wire.native_timeout_seconds(phase) == 120_300
    assert wire.native_event_limit(phase) == 646
    assert wire.native_timeout_seconds({**phase, "maxToolCalls": 2_147_483_647}) == 2_147_483.647
    assert wire.native_timeout_seconds(fetch_phase()) == 600
    assert wire.native_event_limit(fetch_phase()) == 128


@pytest.mark.parametrize("extra", [[], ["github-pr"]])
def test_native_scoped_capabilities_have_canonical_cross_runtime_digest_order(extra):
    supplied = dict(name="remember", objective="Retain reviewed findings in governed memory",
                    capabilities=["network", "memory", "filesystem-write", "filesystem-read", *extra],
                    maxToolCalls=8, executionMode="native")
    expected = dict(name="remember", objective=supplied["objective"],
                    capabilities=sorted(supplied["capabilities"]),
                    requiredToolCalls=[], minToolCalls=0, maxToolCalls=8, freshContext=False, executionMode="native")
    assert wire.encoded(wire.phase(supplied)) == wire.encoded(expected)
    reversed_caps = {**supplied, "capabilities": list(reversed(supplied["capabilities"]))}
    assert wire.digest(wire.phase(reversed_caps)) == wire.digest(expected)
    repo = Path(__file__).resolve().parents[3]
    script = f"""
        import {{missionPhaseDigest}} from {json.dumps((repo / 'mesh-plugin/src/mission-phase.ts').as_uri())};
        import {{readFileSync}} from 'node:fs';
        console.log(JSON.stringify(missionPhaseDigest(JSON.parse(readFileSync(0, 'utf8')))));
    """
    result = subprocess.run(["node", "--experimental-strip-types", "--input-type=module", "-e", script],
                            input=wire.encoded(supplied), capture_output=True, check=True)
    assert json.loads(result.stdout) == wire.digest(expected)
    contract = dict(version=4, reviewedPhase=supplied, phaseDigest=wire.digest(expected), requiredOutputs=[])
    assert wire.contract(contract)["reviewedPhase"] == expected
    for caps in (["memory", "memory"], [*supplied["capabilities"], "shell"]):
        with pytest.raises(ValueError):
            wire.phase({**supplied, "capabilities": caps})
    legacy = {k: v for k, v in supplied.items() if k != "executionMode"}
    with pytest.raises(ValueError, match="memory, github-pr and python-execute require native"):
        wire.phase(legacy)
    with pytest.raises(ValueError, match="memory, github-pr and python-execute require native"):
        wire.phase({**legacy, "capabilities": ["memory"]})


def test_native_headroom_does_not_widen_legacy_authority():
    with pytest.raises(ValueError):
        wire.phase({**fetch_phase(), "maxToolCalls": 128})


def test_native_authority_retains_legacy_digests_and_explicit_obligations():
    legacy = fetch_phase()
    native = wire.phase({**legacy, "executionMode": "native", "requiredToolCalls": []})
    assert list(native)[-1] == "executionMode"
    assert wire.digest(native) != wire.digest(legacy)
    assert wire.reviewed_fetch_arguments(native, {"method": "GET", "url": "https://example.org/chosen"})
    assert not wire.reviewed_fetch_arguments(legacy, {"method": "GET", "url": "https://example.org/chosen"})
    native["requiredToolCalls"] = legacy["requiredToolCalls"]
    native = wire.phase(native)
    observed = dict(model="model", rounds=1, usage=dict(promptTokens=1, completionTokens=1, totalTokens=2),
                    phase=dict(name=native["name"], minToolCalls=0, maxToolCalls=3,
                               attemptedToolCalls=1, successfulToolCalls=1, completedRequiredToolCalls=0))
    with pytest.raises(ValueError):
        wire.evidence(observed, native, success=True)
    observed["phase"]["completedRequiredToolCalls"] = 1
    assert wire.evidence(observed, native, success=True) == observed
    with pytest.raises(ValueError):
        wire.phase({**native, "capabilities": ["filesystem-write"]})


@pytest.mark.parametrize("mode", [None, "future", 1, True])
def test_native_phase_rejects_unknown_or_null_modes(mode):
    with pytest.raises(ValueError):
        wire.phase({**fetch_phase(), "executionMode": mode})


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


@pytest.mark.parametrize("native", [False, True])
@pytest.mark.parametrize("network", [False, True])
@pytest.mark.parametrize("size", ["small", "large", "aggregate"])
def test_canonical_phase_and_inputs_match_actual_typescript_modules(network, native, size):
    _, contract, _, assignment = binding()
    if size != "small":
        sizes = (31553, 26140, 10785) if size == "large" else (128,) * wire.MAX_INPUT_FILES
        original = assignment["inputArtifacts"][0]
        assignment["inputArtifacts"] = wire.inputs([
            dict(original, name=f"research-{i}.md", content="x" * (size - 4) + "\n✓",
                 sha256=wire.text_hash("x" * (size - 4) + "\n✓"))
            for i, size in enumerate(sizes)])
        contract["inputDigest"] = wire.digest(dict(schema="kars.mission-inputs/v1", inputs=assignment["inputArtifacts"]))
    if network:
        contract["reviewedPhase"]["capabilities"].append("network")
        contract["reviewedPhase"]["requiredToolCalls"] = [dict(name="http_fetch", arguments=dict(
            url="https://www.example.com/pricing?q=public%20plans", method="GET"))]
        contract["reviewedPhase"] = wire.phase(contract["reviewedPhase"])
        contract["phaseDigest"] = wire.digest(contract["reviewedPhase"])
    if native:
        contract["reviewedPhase"]["executionMode"] = "native"
        contract["reviewedPhase"]["requiredToolCalls"] = []
        contract["reviewedPhase"] = wire.phase(contract["reviewedPhase"])
        contract["phaseDigest"] = wire.digest(contract["reviewedPhase"])
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


@pytest.mark.parametrize("boundary", ["normal", "base-limit", "base-over", "input-limit", "input-over", "wire-limit", "wire-over"])
def test_full_assignment_budgets_match_typescript(boundary):
    target, contract, admitted, assignment = binding()
    source = assignment["inputArtifacts"][0]["source"]
    documents = wire.inputs([dict(name=f"memory-{i}.md", source=source, content="x" * 63000,
                                  sha256=wire.text_hash("x" * 63000)) for i in range(6)])
    if boundary.startswith(("input-", "wire-")):
        documents.append(dict(documents[0], name="memory-6.md", content="x", sha256=wire.text_hash("x")))
        documents[-1]["content"] += "x" * (wire.MAX_INPUT - len(wire.encoded(documents))
                                            + (boundary == "input-over"))
        documents[-1]["sha256"] = wire.text_hash(documents[-1]["content"])
    contract["inputDigest"] = wire.digest(dict(schema="kars.mission-inputs/v1", inputs=documents))
    assignment.update(contract, inputArtifacts=documents, content="x" * 8192)
    if boundary.startswith("base-"):
        base = {k: v for k, v in assignment.items() if k != "inputArtifacts"}
        assignment["content"] += "x" * (wire.MAX_BASE - len(wire.encoded(base))
                                         + (boundary == "base-over"))
    if boundary.startswith("wire-"):
        assignment["content"] += "x" * (wire.MAX_WIRE - len(wire.encoded(assignment))
                                         + (boundary == "wire-over"))
        assert len(wire.encoded(assignment)) == wire.MAX_WIRE + (boundary == "wire-over")
    else:
        assert len(wire.encoded(assignment)) < wire.MAX_WIRE
    admitted["objectiveDigest"] = wire.text_hash(assignment["content"])
    assert len(wire.encoded(assignment)) > wire.MAX_BASE
    valid = not boundary.endswith("over")
    if valid:
        assert wire.message(wire.encoded(assignment), contract, target, admitted) == assignment
    else:
        with pytest.raises(ValueError):
            wire.message(wire.encoded(assignment), contract, target, admitted)
    repo = Path(__file__).resolve().parents[3]
    script = f"""
        import {{registerHooks}} from 'node:module';
        import {{readFileSync}} from 'node:fs';
        registerHooks({{resolve(specifier, context, next) {{
            return next(specifier.startsWith('./') && specifier.endsWith('.js')
                ? specifier.slice(0, -3) + '.ts' : specifier, context);
        }}}});
        const {{parseMissionMessage}} = await import({json.dumps((repo / 'mesh-plugin/src/mission-protocol.ts').as_uri())});
        console.log(JSON.stringify(parseMissionMessage(JSON.parse(readFileSync(0, 'utf8'))) !== null));
    """
    result = subprocess.run(["node", "--experimental-strip-types", "--input-type=module", "-e", script],
                            input=wire.encoded(assignment), capture_output=True, check=True)
    assert json.loads(result.stdout) is valid


def test_complete_approved_memory_and_new_exports_fit_unchanged_assignment_capacity():
    target, contract, admitted, assignment = binding()
    source = assignment["inputArtifacts"][0]["source"]
    inputs = [dict(name=f"memory-0-document-{i}.md", source=source,
                   content="x" * size, sha256=wire.text_hash("x" * size))
              for i, size in enumerate((29571, 13205, 19474, 43332, 17511, 5728, 4869))]
    inputs[0]["content"] += "x" * (143430 - len(wire.encoded(inputs)))
    inputs[0]["sha256"] = wire.text_hash(inputs[0]["content"])
    assert len(wire.encoded(inputs)) == 143430
    inputs.extend(dict(name=f"peer-{i}.md", source=source, content="y" * size,
                       sha256=wire.text_hash("y" * size))
                  for i, size in enumerate((12000, 7200, 1000)))
    inputs = wire.inputs(inputs)
    contract["inputDigest"] = wire.digest(dict(schema="kars.mission-inputs/v1", inputs=inputs))
    assignment.update(contract, inputArtifacts=inputs)
    assert len(wire.encoded(assignment)) < wire.MAX_BASE
    assert wire.message(wire.encoded(assignment), contract, target, admitted) == assignment
    assignment["inputArtifacts"][0]["content"] = "Substitution"
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
