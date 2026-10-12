# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real pinned Hermes conversation/SDK/tool dispatch; only provider HTTP is fake."""
import json
import importlib.util

import httpx
import pytest

from kars_runtime_hermes.plugin import mission_native as native
from kars_runtime_hermes.plugin import mission_protocol as wire

pytestmark = pytest.mark.skipif(importlib.util.find_spec("run_agent") is None, reason="Requires native pinned Hermes")


@pytest.mark.parametrize("version", ["0.16.1", "0.21.4", "0.21.5rc1", "0.21.6", "1.0.0"])
def test_unqualified_runtime_versions_are_rejected(monkeypatch, version):
    monkeypatch.setattr(native.importlib.metadata, "version", lambda _: version)
    with pytest.raises(ValueError, match="Hermes 0.16.0 or 0.21.5 is required"):
        native.require_supported_runtime()


def response(content="A real native answer", calls=None, usage=True):
    data = {"id": "chatcmpl-local", "object": "chat.completion", "created": 1, "model": "mission-model",
            "choices": [{"index": 0, "finish_reason": "tool_calls" if calls else "stop",
                         "message": {"role": "assistant", "content": content, "tool_calls": calls}}]}
    if usage:
        data["usage"] = {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    return httpx.Response(200, json=data)


def call(name, args):
    return {"id": "call-local", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}


@pytest.fixture
def setup(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    (home / "config.yaml").write_text("model:\n  context_length: 131072\ncompression:\n  enabled: false\n"
                                      "memory:\n  memory_enabled: false\n")
    for key in ("HOME", "HERMES_HOME", "TMPDIR"):
        monkeypatch.setenv(key, str(home))
    monkeypatch.setenv("HERMES_TELEMETRY_ENABLED", "false")
    monkeypatch.setenv("HERMES_DISABLE_LAZY_INSTALLS", "1")
    monkeypatch.setenv("HERMES_SKIP_NODE_BOOTSTRAP", "1")
    monkeypatch.setenv("TIRITH_ENABLED", "false")
    monkeypatch.chdir(tmp_path)
    (tmp_path / "inputs").mkdir()
    (tmp_path / "exports").mkdir()
    phase = wire.phase({"name": "draft", "objective": "Read evidence and write a useful report",
                        "capabilities": ["filesystem-read", "filesystem-write"], "minToolCalls": 0,
                        "maxToolCalls": 4})
    request = {"model": "mission-model", "assignmentId": "local-assignment", "content": "Summarize the evidence",
               "contract": {"version": 4, "reviewedPhase": phase, "phaseDigest": wire.digest(phase),
                            "requiredOutputs": []}}
    return tmp_path, request


def run(setup, responses):
    root, request = setup
    seen, events = [], []

    def provider(req):
        seen.append(json.loads(req.content))
        assert str(req.url) == native.ROUTER + "/chat/completions"
        return responses(len(seen)) if callable(responses) else responses.pop(0)

    result = native.execute(request, root, events.append, transport=httpx.MockTransport(provider))
    return result, seen, events


def test_real_native_conversation_usage(setup):
    result, seen, events = run(setup, [response()])
    assert result["kind"] == "result", result
    assert result["output"] == "A real native answer"
    assert result["evidence"]["usage"]["totalTokens"] == 15
    assert len(seen) == 1
    assert events[0]["evidence"]["rounds"] == 1


def test_native_forwards_correlated_router_activity_and_retains_final_artifact(setup):
    root, request = setup
    request.update(activityFormat=wire.ACTIVITY, artifactFormat="text-v1")
    events, paths = [], []

    def transport(req):
        paths.append(req.url.path)
        if req.url.path == "/v1/chat/completions":
            result = response()
            result.headers.update({"x-kars-telemetry-scope": "native-scope", "x-kars-telemetry-round": "1"})
            return result
        assert req.url.path == "/telemetry/trace"
        assert req.headers["x-kars-service-scope"] == "native-scope"
        return httpx.Response(200, json=dict(
            scope_id="native-scope", rounds=[1], coverage="returned-responses-only",
            durable=False, missing_rounds=[], dropped_events=0, truncated=False,
            events=[dict(kind="round", scope_id="native-scope", round=1, source="router-observed",
                         model="mission-model", http_status=200)]))

    result = native.execute(request, root, events.append, transport=httpx.MockTransport(transport))
    assert result["kind"] == "result", result
    assert result["evidence"]["rounds"] == 1 and result["evidence"]["usage"]["totalTokens"] == 15
    activity = json.loads(events[0]["routerObservations"])
    assert activity["state"] == "observed" and activity["rounds"] == [1]
    assert json.loads(result["artifacts"]["kars-router-observations.json"]) == activity
    assert paths == ["/v1/chat/completions", "/telemetry/trace", "/telemetry/trace"]


def test_native_startup_does_not_install_or_probe_external_endpoints(setup, monkeypatch):
    import socket
    import subprocess

    attempts = []
    installers = []
    popen = subprocess.Popen

    def no_dns(host, *args, **kwargs):
        if host != "127.0.0.1":
            attempts.append(host)
        raise OSError("Native qualification prohibits ambient endpoint discovery")

    def no_installer(args, *rest, **kwargs):
        if isinstance(args, (list, tuple)) and "install" in args:
            installers.append(args)
            raise OSError("Native qualification prohibits runtime installation")
        return popen(args, *rest, **kwargs)

    monkeypatch.setattr(socket, "getaddrinfo", no_dns)
    monkeypatch.setattr(subprocess, "Popen", no_installer)
    _, request = setup
    request["model"] = "gpt-5.4-mini"
    result, seen, _ = run(setup, [response()])
    assert result["kind"] == "result", result
    assert len(seen) == 1
    assert attempts == []
    assert installers == []


def test_real_native_tool_dispatch_named_export(setup):
    root, req = setup
    req["contract"]["requiredOutputs"] = ["report.md"]
    req["contract"]["reviewedPhase"]["minToolCalls"] = 2
    (root / "inputs" / "facts.txt").write_text("Verified local input")
    result, seen, events = run(setup, [
        response(calls=[call("kars_mission_read", {"path": "inputs/facts.txt"})]),
        response(calls=[call("kars_mission_write", {"path": "exports/report.md", "content": "Verified local report"})]),
        response("Report exported")])
    assert result["kind"] == "result", result
    assert result["artifacts"] == {"report.md": "Verified local report"}
    assert result["evidence"]["phase"]["successfulToolCalls"] == 2
    assert len(seen) == 3
    assert len(events) >= 5


def test_native_requires_successful_minimum_after_failed_read_and_export(setup):
    _, req = setup
    req["contract"]["requiredOutputs"] = ["report.md"]
    req["contract"]["reviewedPhase"]["minToolCalls"] = 2
    req["contract"]["reviewedPhase"]["maxToolCalls"] = 8
    result, seen, _ = run(setup, [
        response(calls=[call("kars_mission_read", {"path": "inputs/missing.md"})]),
        response(calls=[call("kars_mission_write", {
            "path": "exports/report.md", "content": "Verified clinic findings"})]),
        response(calls=[call("kars_mission_read", {"path": "exports/report.md"})]),
        response("Exported and verified report.md")])
    assert result["kind"] == "result", result
    assert [request.get("tool_choice") for request in seen[:3]] == ["required"] * 3
    assert seen[3].get("tool_choice") != "required"
    assert "requires 1 successful authorized tool calls; 6 attempts remain" in seen[2]["messages"][-1]["content"]
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 3
    assert result["evidence"]["phase"]["successfulToolCalls"] == 2
    assert result["evidence"]["usage"]["totalTokens"] == 60
    assert result["artifacts"] == {"report.md": "Verified clinic findings"}


def test_native_complete_inputs_are_required_before_exports(setup, input_artifact):
    root, req = setup
    req["inputArtifacts"] = [input_artifact(name, "Complete handoff: " + name) for name in ("first.md", "second.md")]
    req["contract"]["requiredOutputs"] = ["report.md", "checklist.md"]
    for name in ("first.md", "second.md"):
        (root / "inputs" / name).write_text("Complete handoff: " + name)
    result, seen, _ = run(setup, [
        response(calls=[call("kars_mission_read", {"path": "inputs/first.md"})]),
        response(calls=[call("kars_mission_read", {"path": "inputs/second.md"})]),
        response(calls=[call("kars_mission_write", {"path": "exports/report.md", "content": "Combined findings"})]),
        response(calls=[call("kars_mission_write", {"path": "exports/checklist.md", "content": "Combined actions"})]),
        response("Both handoffs used")])
    assert result["kind"] == "result", result
    assert result["evidence"]["phase"]["successfulToolCalls"] == 4
    assert seen[0]["tool_choice"]["function"]["name"] == "kars_mission_read"
    assert seen[1]["tools"][0]["function"]["parameters"]["properties"]["path"]["enum"] == ["inputs/second.md"]
    assert all(seen[n]["parallel_tool_calls"] is False for n in (0, 1))
    assert set(result["artifacts"]) == {"report.md", "checklist.md"}


def test_native_impossible_input_export_allowance_spends_no_tokens(setup, input_artifact):
    _, req = setup
    req["inputArtifacts"] = [input_artifact("first.md"), input_artifact("second.md")]
    req["contract"]["requiredOutputs"] = ["report.md", "checklist.md"]
    req["contract"]["reviewedPhase"]["maxToolCalls"] = 2
    result, seen, _ = run(setup, [])
    assert result["kind"] == "error" and "Insufficient reviewed tool calls" in result["error"]
    assert result["evidence"]["rounds"] == 0
    assert seen == []


def test_native_final_answer_cannot_replace_input_reads(setup, input_artifact):
    _, req = setup
    req["inputArtifacts"] = [input_artifact("facts.md")]
    result, _, _ = run(setup, [response("Pretend the input was read")])
    assert result["kind"] == "error" and "Unread immutable inputs" in result["error"]


def test_native_bundle_fits_eight_inputs_and_three_exports_in_original_limits(setup, input_artifact):
    root, req = setup
    inputs = [input_artifact(f"input-{n}.md", f"Complete historical record {n}\r\n✓") for n in range(8)]
    req["inputArtifacts"] = inputs
    for item in inputs:
        (root / "inputs" / item["name"]).write_bytes(item["content"].encode())
    outputs = ["brief.md", "checklist.md", "decisions.md"]
    req["contract"]["requiredOutputs"] = outputs
    req["contract"]["reviewedPhase"].update(minToolCalls=2, maxToolCalls=4)
    result, seen, _ = run(setup, [
        response(calls=[call("kars_mission_read", {"path": "inputs"})]),
        *[response(calls=[call("kars_mission_write", {
            "path": "exports/" + name, "content": "Substantive synthesis for " + name})]) for name in outputs],
        response("All required outputs exported")])
    assert result["kind"] == "result", result
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 4
    assert result["evidence"]["phase"]["successfulToolCalls"] == 4
    assert len(seen) == 5 <= native.MAX_CALLS
    assert seen[0]["tools"][0]["function"]["parameters"]["properties"]["path"]["enum"] == ["inputs"]
    read_reply = next(message for message in seen[1]["messages"] if message["role"] == "tool")
    assert json.loads(json.loads(read_reply["content"])["content"]) == inputs
    assert set(result["artifacts"]) == set(outputs)


@pytest.mark.parametrize("tool", ["delegate_task", "terminal", "memory", "clarify", "unknown"])
def test_native_special_dispatch_is_blocked(setup, tool):
    result, seen, _ = run(setup, [response(calls=[call(tool, {})])])
    assert result["kind"] == "error"
    assert "Unsupported native tool" in result["error"]
    assert len(seen) == 1
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 0


def test_native_tool_budget_before_batch(setup):
    setup[1]["contract"]["reviewedPhase"]["maxToolCalls"] = 0
    result, seen, _ = run(setup, [response(calls=[call("kars_mission_read", {"path": "inputs/a"})])])
    assert result["kind"] == "error" and "limit" in result["error"]
    assert len(seen) == 1


def test_unknown_provider_usage_never_success(setup):
    result, seen, _ = run(setup, [response(usage=False)])
    assert result["kind"] == "error"
    assert result["evidence"]["usage"] is None
    assert len(seen) == 1


def test_required_export_cannot_be_stdout(setup):
    setup[1]["contract"]["requiredOutputs"] = ["report.md"]
    result, _, _ = run(setup, [response("Here is report.md: made up")])
    assert result["kind"] == "error" and "exports missing" in result["error"]


def test_native_finalization_measured_and_bounded(setup):
    setup[1]["contract"]["reviewedPhase"]["maxToolCalls"] = 32
    result, seen, events = run(setup, lambda n: response(
        "Final answer" if n == native.MAX_CALLS else None,
        calls=None if n == native.MAX_CALLS else [call("kars_mission_read", {"path": "inputs/absent"})]))
    assert len(seen) == native.MAX_CALLS, result
    assert result["evidence"]["rounds"] == native.MAX_CALLS
    assert result["evidence"]["usage"]["totalTokens"] == 15 * native.MAX_CALLS
    assert result["kind"] == "error" and result["error"] == "Native conversation did not complete"
    assert not {"tools", "tool_choice", "parallel_tool_calls"} & seen[-1].keys()


@pytest.mark.parametrize("path", ["../escape", "/etc/passwd", "inputs/facts.txt", "exports/../escape",
                                   "exports/response.md"])
def test_filesystem_write_confinement(setup, path):
    root, req = setup
    (root / "inputs" / "facts.txt").write_text("Immutable")
    result, _, _ = run(setup, [response(calls=[call("kars_mission_write", {"path": path, "content": "overwrite"})]),
                             response()])
    assert result["evidence"]["phase"]["successfulToolCalls"] == 0
    assert (root / "inputs" / "facts.txt").read_text() == "Immutable"


@pytest.mark.parametrize("failure", ["oversize", "invalid-utf8", "directory", "symlink", "fifo", "count", "aggregate"])
def test_real_native_rejects_invalid_exports(setup, failure):
    import os

    root, _ = setup
    path = root / "exports" / "report.md"
    if failure == "oversize":
        path.write_bytes(b"a" * 65537)
    elif failure == "invalid-utf8":
        path.write_bytes(b"\xff")
    elif failure == "directory":
        path.mkdir()
    elif failure == "symlink":
        (root / "outside").write_text("not an export")
        path.symlink_to(root / "outside")
    elif failure == "fifo":
        os.mkfifo(path)
    elif failure == "count":
        for n in range(17):
            (path.parent / f"report-{n}.txt").write_text("a")
    else:
        path.write_bytes(b"a" * 65536)
        (path.parent / "other.txt").write_bytes(b"b" * 65536)
    result, _, _ = run(setup, [response()])
    assert result["kind"] == "error"


@pytest.mark.parametrize("mode,path_type", [("read", "symlink"), ("write", "symlink"),
                                            ("read", "fifo"), ("write", "fifo"), ("write", "directory")])
def test_native_tools_refuse_nonregular_paths(setup, mode, path_type):
    import os

    root, _ = setup
    outside = root / "outside"
    outside.write_text("unchanged")
    path = root / "exports" / "report.md"
    if path_type == "symlink":
        path.symlink_to(outside)
    elif path_type == "fifo":
        os.mkfifo(path)
    else:
        path.mkdir()
    args = {"path": "exports/report.md"}
    if mode == "write":
        args["content"] = "overwrite"
    result, _, _ = run(setup, [response(calls=[call("kars_mission_" + mode, args)]), response()])
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == 0
    assert outside.read_text() == "unchanged"


@pytest.mark.parametrize("problem", ["status", "non-json", "missing", "bool", "negative", "sum", "oversize"])
def test_provider_failures_never_become_measured_success(setup, problem):
    value = response()
    if problem == "status":
        value = httpx.Response(503, json={"error": "unavailable"})
    elif problem == "non-json":
        value = httpx.Response(200, content=b"not json")
    elif problem == "oversize":
        value = httpx.Response(200, content=b"a" * (native.MAX_RESPONSE + 1))
    else:
        data = value.json()
        if problem == "missing":
            del data["usage"]["prompt_tokens"]
        elif problem == "bool":
            data["usage"]["prompt_tokens"] = True
        elif problem == "negative":
            data["usage"]["completion_tokens"] = -5
        else:
            data["usage"]["total_tokens"] = 500
        value = httpx.Response(200, json=data)
    result, seen, _ = run(setup, [value])
    assert result["kind"] == "error" and result["evidence"]["usage"] is None
    assert len(seen) == 1


@pytest.mark.parametrize("problem", ["status", "usage", "tools", "allowed-tools"])
def test_native_finalization_failure_cannot_retry_past_call_bound(setup, problem):
    setup[1]["contract"]["reviewedPhase"]["maxToolCalls"] = 32

    def provider(n):
        if n < native.MAX_CALLS:
            return response(calls=[call("kars_mission_read", {"path": "inputs/absent"})])
        if problem == "status":
            return httpx.Response(503, json={"error": "unavailable"})
        if problem == "usage":
            return response(usage=False)
        if problem == "allowed-tools":
            return response(calls=[call("kars_mission_read", {"path": "inputs/absent"})])
        return response(calls=[call("terminal", {"command": "must-not-run"})])

    result, seen, _ = run(setup, provider)
    assert result["kind"] == "error"
    assert len(seen) == native.MAX_CALLS
    assert result["evidence"]["rounds"] == native.MAX_CALLS
    assert not {"tools", "tool_choice", "parallel_tool_calls"} & seen[-1].keys()


def test_usage_cannot_recover_after_unmeasured_round(setup):
    root, _ = setup
    (root / "inputs" / "facts.txt").write_text("Evidence")
    result, seen, _ = run(setup, [response(calls=[call("kars_mission_read", {"path": "inputs/facts.txt"})],
                                         usage=False), response()])
    assert result["kind"] == "error" and result["evidence"]["usage"] is None
    assert len(seen) == 2


def test_successful_tools_minimum_not_attempts(setup):
    setup[1]["contract"]["reviewedPhase"]["minToolCalls"] = 1
    result, _, _ = run(setup, [response(calls=[call("kars_mission_read", {"path": "inputs/missing"})]), response()])
    assert result["kind"] == "error"
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == 0
