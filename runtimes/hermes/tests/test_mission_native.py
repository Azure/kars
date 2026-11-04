# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Real pinned Hermes conversation/SDK/tool dispatch; only router HTTP is fake."""
import hashlib
import json
import importlib.util
import os
import sys

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
    request = {"model": "mission-model", "modelLimits": {"contextWindow": 1_050_000, "maxTokens": 128_000},
               "assignmentId": "local-assignment", "content": "Summarize the evidence",
               "contract": {"version": 4, "reviewedPhase": phase, "phaseDigest": wire.digest(phase),
                            "requiredOutputs": []}}
    return tmp_path, request


def run(setup, responses):
    root, request = setup
    seen, events = [], []

    def provider(req):
        if req.url.path == "/agt/evaluate":
            assert json.loads(req.content)["action"] in ("tool:kars_mission_read", "tool:kars_mission_write")
            return httpx.Response(200, json={"allowed": True})
        seen.append(json.loads(req.content))
        assert str(req.url) == native.ROUTER + "/chat/completions"
        assert req.extensions["timeout"] == {"connect": 5, "read": 630, "write": 10, "pool": 5}
        if not seen[-1].get("tools") and "tool_choice" in seen[-1]:
            return httpx.Response(400, json={"error": {"param": "tool_choice"}})
        return responses(len(seen)) if callable(responses) else responses.pop(0)

    result = native.execute(request, root, events.append, transport=httpx.MockTransport(provider))
    return result, seen, events


@pytest.mark.parametrize("answer", ["I could not access the required source; exact-host approval is needed.", "🔥" * 5000])
def test_missing_export_retains_bounded_final_explanation_without_success(setup, answer):
    setup[1]["contract"]["requiredOutputs"] = ["report.md"]
    result, _, _ = run(setup, [response(answer)])
    assert result["kind"] == "error" and result["error"] == "Required exports missing"
    assert result["output"] == answer[:4096]
    assert len(result["output"].encode()) <= 16384
    assert "artifacts" not in result


@pytest.mark.parametrize("charter", [None, "", 'Verified facts: café\nUse file_read; preserve quoted "facts".'])
@pytest.mark.parametrize("native_mode", [False, True])
def test_reviewed_sandbox_charter_reaches_model_without_changing_phase_authority(setup, monkeypatch, charter, native_mode):
    if charter is None:
        monkeypatch.delenv("KARS_AGENT_INSTRUCTIONS", raising=False)
    else:
        monkeypatch.setenv("KARS_AGENT_INSTRUCTIONS", charter)
    request = setup[1]
    if native_mode:
        phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native"})
        peers = dict(rootUid="reviewed-root", stateName="kars-reviewed-" + "a" * 40, slot="r0-p0",
                     peers=[dict(role="writer", phase="draft", slot="r0-p0", agentName="rp-" + "a" * 20 + "-r0-p0")])
        request["contract"] = wire.contract(dict(version=5, reviewedPhase=phase, phaseDigest=wire.digest(phase),
                                                requiredOutputs=[], peerContext=peers))
    original = json.dumps(request, sort_keys=True)
    result, seen, _ = run(setup, [response()])
    assert result["kind"] == "result", result
    prompt = "\n".join(str(message.get("content", "")) for message in seen[0]["messages"])
    assert request["content"] in prompt
    if charter:
        assert charter in prompt
        assert "Reviewed sandbox charter (subject to the current assignment and phase boundaries):" in prompt
    else:
        assert "Reviewed sandbox charter" not in prompt
    assert native.ASSIGNMENT_PRECEDENCE in prompt
    assert json.dumps(request, sort_keys=True) == original
    assert not any(tool["function"]["name"] == "terminal" for tool in seen[0].get("tools", []))


def test_native_model_capacity_reaches_provider(setup):
    result, seen, _ = run(setup, [response()])
    assert result["kind"] == "result", result
    assert seen[0].get("max_tokens", seen[0].get("max_completion_tokens")) == 128_000


@pytest.mark.parametrize("limits", [None, {}, {"contextWindow": 100, "maxTokens": True},
                                    {"contextWindow": 0, "maxTokens": 128_000}])
def test_native_refuses_missing_or_invalid_model_capacity(setup, limits):
    setup[1]["modelLimits"] = limits
    result, seen, _ = run(setup, [])
    assert result["kind"] == "error"
    assert seen == []


FETCH_ARGS = {"method": "GET", "url": "https://www.example.com/pricing"}
FETCH_PAGE = {"status": 200, "headers": {"content-type": "text/html"},
              "body": "<p>Official published terms</p>", "truncated": False}


@pytest.mark.parametrize("work", [
    "Feedback: reuse the retained package; add an action sheet; do not restart broad research.",
    "Assigned work: prepare a decision meeting from approved memory. No new external research.",
    "Assigned work: research public Kind and Microsoft Learn documentation through approved hosts and cite exact URLs.",
])
def test_native_prompt_preserves_objective_and_marks_peer_locators_as_nonauthority(setup, work):
    _, request = setup
    request["content"] += ". " + work
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native",
                        "objective": "Independently inspect primary documentation through approved public hosts"})
    peers = dict(rootUid="reviewed-root", stateName="kars-reviewed-" + "a" * 40, slot="r0-p0",
                 peers=[dict(role="writer", phase="draft", slot="r0-p0", agentName="rp-" + "a" * 20 + "-r0-p0")])
    request["contract"] = wire.contract(dict(version=5, reviewedPhase=phase, phaseDigest=wire.digest(phase),
                                            requiredOutputs=[], peerContext=peers))
    result, chats, _ = run(setup, lambda _: response())
    assert result["kind"] == "result", result
    prompt = "\n".join(str(message.get("content", "")) for message in chats[0]["messages"])
    assert request["content"] in prompt and phase["objective"] in prompt
    assert native.ASSIGNMENT_PRECEDENCE in prompt
    assert any(native.ASSIGNMENT_PRECEDENCE in message.get("content", "")
               for message in chats[0]["messages"] if message["role"] == "system")
    assert "References to customer-facing filenames in the charter or revision feedback do not rename or waive this phase's required exports" in prompt
    assert "write its exact required export names without waiting for another approval" in prompt
    assert "An internal handoff is not the final customer deliverable" in prompt
    assert "Do not invent additional task restrictions" in prompt
    assert "For document-only work" not in prompt
    assert "never expands reviewed capabilities" in prompt
    assert "report the conflict without executing it or claiming completion" in prompt
    assert "later phases produce their own deliverables" in prompt
    assert "original artifact names from the provenance manifest" in prompt
    assert "reference evidence, not approved knowledge or new permissions" in prompt
    assert peers["peers"][0]["agentName"] in prompt
    assert "not proof of readiness, identity, or permission" in prompt
    assert not any("mesh" in tool["function"]["name"] for tool in chats[0].get("tools", []))


@pytest.mark.parametrize("slot", [None, "r0-p0", "synthesis"])
@pytest.mark.parametrize("artifact", ["handoff.md", "mission-acceptance-checklist.md"])
def test_revision_final_filename_feedback_preserves_reviewed_export_contract(setup, artifact, slot):
    root, request = setup
    request["content"] = (
        "Revise mission-acceptance-checklist.md: remove its mistaken handoff.md self-reference. "
        "Keep the final concise and save it with the same required name."
    )
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native"})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase), requiredOutputs=["handoff.md"])
    if slot:
        peers = dict(rootUid="reviewed-root", stateName="kars-reviewed-" + "a" * 40, slot=slot,
                     peers=[dict(role="writer", phase="draft", slot="r0-p0", agentName="rp-" + "a" * 20 + "-r0-p0")])
        request["contract"] = wire.contract({**request["contract"], "version": 5, "peerContext": peers})
    chats = []

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            assert payload["action"] == "tool:write_file"
            return httpx.Response(200, json={"allowed": True})
        chats.append(payload)
        if len(chats) == 1:
            return response(calls=[call("write_file", {"path": f"exports/{artifact}", "content": "Phase findings for the corrected checklist"})])
        return response("Phase findings saved")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    if artifact == "handoff.md":
        assert result["kind"] == "result", result
        assert result["artifacts"] == {"handoff.md": "Phase findings for the corrected checklist"}
    else:
        assert result["kind"] == "error" and result["error"] == "Required exports missing"
        assert not (root / "exports/handoff.md").exists()
    prompt = "\n".join(str(message.get("content", "")) for message in chats[0]["messages"])
    assert request["content"] in prompt and native.ASSIGNMENT_PRECEDENCE in prompt
    assert "Requests to keep the final filename or deliver only the final document apply to final synthesis; they do not prohibit internal phase handoffs" in prompt
    assert "If the assignment explicitly prohibits a required export, report the conflict without writing it or claiming completion" in prompt
    assert "handoff.md" in json.dumps(chats[-1]["messages"])
    if slot:
        stage = "Execution stage: final synthesis." if slot == "synthesis" else 'Execution stage: internal reviewed phase "r0-p0".'
        assert any(message.get("content", "").startswith(stage) for message in chats[0]["messages"])
        assert f'Your required {"customer" if slot == "synthesis" else "internal"} output names are ["handoff.md"]' in prompt
        if slot == "synthesis":
            assert "A previous revision is reference material, not evidence that this run performed its claimed actions" in prompt
            assert "Check that the final file, not merely your response, contains the corrections" in prompt
        else:
            assert 'Your assigned role is "writer", phase "draft"' in prompt
            assert f'your own discovery name is "rp-{"a" * 20}-r0-p0"' in prompt
            assert "That entry is you, not another specialist" in prompt
            assert "If multiple fresh registrations match, do not guess" in prompt
            assert "Required names are minimum handoff obligations, not an exclusive output list" in prompt
            assert "A path or hash in a handoff and an execution receipt do not retain file contents" in prompt
            assert "After testing, use write_file under exports/ for each additional file" in prompt
            assert "do not re-export unchanged revision or research inputs" in prompt
            assert "Preserve required handoffs, artifact limits, and this phase's authority. Report failed tests as failed" in prompt
    else:
        assert "Execution stage:" not in prompt


def test_native_mode_uses_real_registry_writer_without_input_order(setup, input_artifact):
    root, request = setup
    (root / "scratch").mkdir()
    item = input_artifact("facts.md", "Facts need not be read before an independent draft")
    (root / "inputs/facts.md").write_text(item["content"])
    request["inputArtifacts"] = [item]
    request["contract"]["requiredOutputs"] = ["report.md"]
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native"})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    chats, policies = [], []

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            policies.append(payload)
            return httpx.Response(200, json={"allowed": True})
        chats.append(payload)
        assert any(native.ASSIGNMENT_PRECEDENCE in message.get("content", "")
                   for message in payload["messages"] if message["role"] == "system")
        assert "kars_mission_read" not in json.dumps(payload.get("tools"))
        assert {t["function"]["name"] for t in payload.get("tools", [])} == {"kars_access_request", "read_file", "write_file", "search_files"}
        assert payload.get("tool_choice") not in ("required", {"type": "function", "function": {"name": "read_file"}})
        return response(calls=[call("write_file", {"path": "exports/report.md", "content": "Native persisted report"})]) if len(chats) == 1 else response("Report saved")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "result", result
    assert result["artifacts"] == {"report.md": "Native persisted report"}
    assert policies[0]["action"] == "tool:write_file"
    assert result["evidence"]["phase"]["successfulToolCalls"] == 1
    assert (root / "inputs/facts.md").read_text() == item["content"]


@pytest.mark.parametrize("source", ["input", "owned"])
def test_native_registry_copies_retained_source_without_transcription(setup, input_artifact, source):
    root, request = setup
    (root / "scratch").mkdir()
    content = "\ufeff# café\r\nprint('雪')\r\n"
    item = input_artifact("app.py", content)
    if source == "input":
        (root / "inputs/app.py").write_bytes(content.encode())
        request["inputArtifacts"] = [item]
    request["contract"]["requiredOutputs"] = ["app.py"]
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native"})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    args = {"path": "exports/app.py", "source_path": "inputs/app.py" if source == "input" else "scratch/app.py", "sha256": item["sha256"]}
    calls = ([{"path": "scratch/app.py", "content": content}] if source == "owned" else []) + [args]
    chats, policies = [], []

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            policies.append(payload)
            return httpx.Response(200, json={"allowed": True})
        chats.append(payload)
        schema = next(t["function"] for t in payload["tools"] if t["function"]["name"] == "write_file")
        assert "source_path" in schema["parameters"]["properties"]
        assert "content" not in schema["parameters"]["required"]
        assert "without model transcription" in schema["description"]
        if calls:
            tool_call = call("write_file", calls.pop(0))
            tool_call["id"] = f"copy-call-{len(chats)}"
            return response(calls=[tool_call])
        return response("Retained bytes delivered")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "result", result
    assert result["artifacts"]["app.py"].encode() == content.encode()
    assert (root / "exports/app.py").read_bytes() == content.encode()
    assert policies[-1]["context"]["arguments"] == args
    assert result["evidence"]["phase"]["successfulToolCalls"] == (2 if source == "owned" else 1)


def test_native_prompt_input_paths_read_both_handoffs_within_reviewed_calls(setup, input_artifact):
    root, request = setup
    items = [input_artifact("r0-p0.md", "Research evidence"), input_artifact("r1-p0.md", "Draft evidence")]
    for item in items:
        (root / "inputs" / item["name"]).write_text(item["content"])
    request["inputArtifacts"] = items
    request["contract"]["requiredOutputs"] = ["report.md"]
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native", "maxToolCalls": 3})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    chats, paths = [], []

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            return httpx.Response(200, json={"allowed": True})
        chats.append(payload)
        if len(chats) == 1:
            prompt = "\n".join(str(message.get("content", "")) for message in payload["messages"])
            prefix = "Immutable input files (read using the exact path, not the artifact name): "
            manifest = json.loads(next(line[len(prefix):] for line in prompt.splitlines() if line.startswith(prefix)))
            assert manifest == [{"name": item["name"], "path": "inputs/" + item["name"],
                                 "sha256": item["sha256"]} for item in items]
            paths.extend(item["path"] for item in manifest)
        if len(chats) <= 2:
            return response(calls=[call("read_file", {"path": paths[len(chats) - 1]})])
        if len(chats) == 3:
            return response(calls=[call("write_file", {"path": "exports/report.md", "content": "Research and draft reconciled"})])
        return response("Both handoffs read and report exported")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "result", result
    assert result["artifacts"] == {"report.md": "Research and draft reconciled"}
    assert result["evidence"]["phase"]["successfulToolCalls"] == 3
    for item in items:
        assert item["content"] in json.dumps(chats[-1]["messages"])
        assert (root / "inputs" / item["name"]).read_text() == item["content"]


def test_native_human_answer_continues_same_conversation_and_restores_registration(setup):
    from tools.registry import registry

    root, request = setup
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native"})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase), requiredOutputs=["report.md"])
    previous = registry.get_entry("kars_access_request")
    chats, reports, access_calls = [], [], []
    answer = "Use the internal report, explicitly label the external source unavailable."
    entry = {"scope_id": "human-scope", "request_id": "request-human", "kind": "clarification", "status": "pending"}

    def provider(req):
        if req.url.path == "/v1/access-request":
            access_calls.append(json.loads(req.content))
            return httpx.Response(202, json={"status": "queued", "request": entry})
        if req.url.path.endswith("/wait"):
            assert req.url.params["scope_id"] == "human-scope"
            return httpx.Response(200, json={**entry, "status": "approved", "answer": answer})
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            assert payload["context"]["scope_id"] == "human-scope"
            return httpx.Response(200, json={"allowed": True})
        if req.url.path == "/telemetry/tool":
            reports.append(payload)
            return httpx.Response(200, json={"recorded": True, "source": "harness-reported", "enforcement_changed": False})
        assert req.url.path == "/v1/chat/completions"
        chats.append(payload)
        if len(chats) == 1:
            result = response(calls=[{**call("kars_access_request", {"operation": "request", "kind": "clarification",
                "target": "source-choice", "reason": "Which source should I use?"}), "id": "ask-human"}])
        elif len(chats) == 2:
            result_message = next(m["content"] for m in payload["messages"] if m.get("tool_call_id") == "ask-human")
            assert json.loads(result_message) == {"request": {**entry, "status": "approved", "answer": answer}, "enforcement_changed": False}
            result = response(calls=[{**call("write_file", {"path": "exports/report.md", "content": answer}), "id": "write-answer"}])
        else:
            result = response("Saved the report following the human clarification.")
        result.headers.update({"x-kars-telemetry-scope": "human-scope", "x-kars-telemetry-round": str(len(chats))})
        return result

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "result", result
    assert result["artifacts"] == {"report.md": answer}
    assert result["evidence"]["phase"]["successfulToolCalls"] == 2
    assert len(access_calls) == 1 and access_calls[0]["scope_id"] == "human-scope"
    assert [(r["call_id"], r["ok"]) for r in reports] == [("ask-human", True), ("write-answer", True)]
    assert registry.get_entry("kars_access_request") is previous


def test_native_large_fetch_spills_then_reads_late_evidence_and_exports(setup):
    from pathlib import Path
    from tools import tool_result_storage
    from kars_runtime_hermes.plugin.mission_spill import CHUNK_CHARS, DIRECTORY

    root, request = setup
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native",
                        "capabilities": ["network", "filesystem-read", "filesystem-write"]})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase), requiredOutputs=["report.md"])
    marker = "LATE-VERIFIED-SOURCE-EVIDENCE"
    envelope = {**FETCH_PAGE, "body": "x" * 150000 + marker * 4 + "雪😀\nEnd of complete source"}
    original = json.dumps(envelope)
    original_writer = tool_result_storage._write_to_spillover
    chats, fetches, policies = [], [], []
    spilled = []

    def named_call(name, args, call_id):
        return {**call(name, args), "id": call_id}

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            policies.append(payload)
            return httpx.Response(200, json={"allowed": True})
        if req.url.path == "/egress/fetch":
            fetches.append(payload)
            return httpx.Response(200, json=envelope)
        chats.append(payload)
        if len(chats) == 1:
            return response(calls=[named_call("http_fetch", FETCH_ARGS, "large-fetch")])
        if len(chats) == 2:
            result = next(message["content"] for message in payload["messages"]
                          if message.get("tool_call_id") == "large-fetch")
            assert tool_result_storage.PERSISTED_OUTPUT_TAG in result
            assert marker not in result
            path = Path(tool_result_storage.extract_persisted_path(result))
            assert path.parent == root / DIRECTORY
            assert path.stat().st_size > 65536
            records = [json.loads(line) for line in path.read_text().splitlines()]
            assert "".join(record["text"] for record in records[1:]) == original
            spilled.append(path)
            offset = original.index(marker) // CHUNK_CHARS + 2
            return response(calls=[named_call("read_file", {"path": str(path), "offset": offset, "limit": 8},
                                              "read-late-page")])
        if len(chats) == 3:
            page = next(message["content"] for message in payload["messages"]
                        if message.get("tool_call_id") == "read-late-page")
            assert marker in page, page
            assert tool_result_storage.PERSISTED_OUTPUT_TAG not in page
            assert "outside the assignment" not in page
            return response(calls=[named_call("write_file", {"path": "exports/report.md",
                "content": "Verified late source evidence: " + marker}, "export-report")])
        return response("Verified the saved source without refetching and wrote the report.")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "result", result
    assert result["artifacts"] == {"report.md": "Verified late source evidence: " + marker}
    assert result["evidence"]["phase"]["successfulToolCalls"] == 3
    assert len(fetches) == 1 and len(chats) == 4 and len(spilled) == 1
    assert {item["action"] for item in policies} == {"tool:http_fetch", "tool:read_file", "tool:write_file"}
    assert not (root / "home/cache/spillover").exists()
    assert tool_result_storage._write_to_spillover is original_writer


def test_native_turn_budget_uses_assignment_writer_and_restores_it(setup, monkeypatch):
    from pathlib import Path
    from agent import tool_executor
    from tools import tool_result_storage
    from kars_runtime_hermes.plugin import mission_spill

    from types import SimpleNamespace

    root, _ = setup
    original_writer = tool_result_storage._write_to_spillover
    original_host_check = tool_result_storage._is_host_side_env
    store = mission_spill.MissionSpill(root)
    no_shell = SimpleNamespace(execute=lambda *_args, **_kwargs: pytest.fail("Spill attempted a shell command"))
    try:
        store.install()
        messages = [{"role": "tool", "tool_call_id": "turn-result", "content": "turn evidence" * 30000}]
        tool_executor.enforce_turn_budget(messages, env=no_shell)
        path = Path(tool_result_storage.extract_persisted_path(messages[0]["content"]))
        assert path in store.files
        monkeypatch.setattr(mission_spill, "MAX_FILES", 1)
        fallback = tool_executor.maybe_persist_tool_result("exhausted" * 30000, "http_fetch", "next-result",
                                                          env=no_shell)
        assert "Full output could not be saved" in fallback
        assert tool_result_storage.extract_persisted_path(fallback) is None
        assert path.exists() and len(store.files) == 1
        assert not (root / "home/cache/spillover").exists()
    finally:
        store.close()
    assert tool_result_storage._write_to_spillover is original_writer
    assert tool_result_storage._is_host_side_env is original_host_check


@pytest.mark.parametrize("deny_handoff", [False, True])
@pytest.mark.parametrize("empty_final", [False, True])
def test_native_budget_reminders_track_verified_exports_without_forcing_tools(setup, deny_handoff, empty_final):
    root, request = setup
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native", "maxToolCalls": 3})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase), requiredOutputs=["handoff.md"])
    chats = []
    calls = [call("read_file", {"path": "inputs/absent.md"}),
             call("write_file", {"path": "exports/root-brief.md", "content": "Root output is not the phase handoff"}),
             call("write_file", {"path": "exports/handoff.md", "content": "Substantive native handoff"})]

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            denied = deny_handoff and payload["context"]["arguments"].get("path") == "exports/handoff.md"
            return httpx.Response(200, json={"allowed": not denied})
        chats.append(payload)
        remaining = max(0, 4 - len(chats))
        reminder = payload["messages"][-1]["content"]
        assert f"Only {remaining} reviewed tool attempts remain" in reminder
        missing = '["handoff.md"]' if len(chats) <= 3 or deny_handoff else "[]"
        assert f"Missing required phase exports: {missing}" in reminder
        assert "Root deliverables and conversational prose do not replace" in reminder
        if len(chats) <= 3:
            assert {tool["function"]["name"] for tool in payload["tools"]} == {"kars_access_request", "read_file", "write_file", "search_files"}
            assert payload.get("tool_choice") in (None, "auto")
            assert payload["parallel_tool_calls"] is False
            return response(calls=[calls[len(chats) - 1]])
        assert not {"tools", "parallel_tool_calls", "tool_choice"} & payload.keys()
        assert "No further tool calls are permitted" in reminder
        return response(None if empty_final and len(chats) == 4 else "Only the persisted files are delivered")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 3
    assert len(chats) == (5 if empty_final else 4)
    if deny_handoff:
        assert result["kind"] == "error" and result["error"] == "Required exports missing"
        assert not (root / "exports/handoff.md").exists()
    else:
        assert result["kind"] == "result", result
        assert result["artifacts"]["handoff.md"] == "Substantive native handoff"


@pytest.mark.parametrize("missing_required", [False, True])
def test_native_failed_copy_replacement_preserves_required_export_error_precedence(setup, missing_required):
    root, request = setup
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native", "maxToolCalls": 2})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase),
                               requiredOutputs=["missing.md" if missing_required else "handoff.md"])
    chats = []
    original = "Earlier handoff is not a successful replacement"
    calls = [call("write_file", {"path": "exports/handoff.md", "content": original}),
             call("write_file", {"path": "exports/handoff.md", "source_path": "exports/handoff.md",
                                 "sha256": "sha256:" + "0" * 64})]
    for index, proposed in enumerate(calls):
        proposed["id"] = f"replacement-{index}"

    def provider(req):
        if req.url.path == "/agt/evaluate":
            return httpx.Response(200, json={"allowed": True})
        payload = json.loads(req.content)
        chats.append(payload)
        if len(chats) <= len(calls):
            return response(calls=[calls[len(chats) - 1]])
        assert len(chats) == 3
        tool_result = next(message for message in payload["messages"]
                           if message.get("tool_call_id") == "replacement-1")
        assert "exact sha256" in tool_result["content"]
        return response("The replacement failed; do not deliver the earlier handoff as success")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "error"
    assert result["error"] == ("Required exports missing" if missing_required else "Unresolved named output write failure")
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 2
    assert result["evidence"]["phase"]["successfulToolCalls"] == 1
    assert (root / "exports/handoff.md").read_text() == original
    assert "artifacts" not in result


@pytest.mark.parametrize("mode,network,bridge", [
    ("native", False, True), ("native", True, False), ("reviewed", True, True)])
def test_peer_registration_requires_native_network_bridge(setup, mode, network, bridge):
    from unittest.mock import Mock
    from kars_runtime_hermes.plugin.mission_peer import SCHEMAS

    root, request = setup
    phase = {**request["contract"]["reviewedPhase"]}
    if mode == "native":
        phase["executionMode"] = "native"
    if network:
        phase["capabilities"] = [*phase["capabilities"], "network"]
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    callback = Mock(side_effect=AssertionError("No peer forwarding allowed"))

    def provider(req):
        body = json.loads(req.content)
        assert not set(SCHEMAS).intersection(t["function"]["name"] for t in body.get("tools", []))
        return response()

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider),
                            peer_call=callback if bridge else None)
    assert result["kind"] == "result", result
    callback.assert_not_called()


@pytest.mark.parametrize("allowed", [False, True])
def test_actual_native_peer_policy_and_registration_cleanup(setup, allowed):
    from unittest.mock import Mock
    from tools.registry import registry
    from kars_runtime_hermes.plugin.mission_peer import SCHEMAS

    root, request = setup
    phase = {**request["contract"]["reviewedPhase"], "executionMode": "native",
             "capabilities": ["network"]}
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    previous = {name: registry.get_entry(name) for name in SCHEMAS}
    callback = Mock(return_value={"messages": [], "count": 0})
    chats = []

    def provider(req):
        body = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            assert body["action"] == "tool:kars_mesh_inbox"
            assert body["context"] == {"tool_call_id": "call-local", "tool": "kars_mesh_inbox", "arguments": {}}
            return httpx.Response(200, json={"allowed": allowed})
        chats.append(body)
        if len(chats) == 1:
            return response(calls=[call("kars_mesh_inbox", {})])
        result = json.loads(next(m["content"] for m in body["messages"] if m["role"] == "tool"))
        assert (result == {"messages": [], "count": 0}) if allowed else "refused" in result["error"]
        return response()

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider), peer_call=callback)
    assert result["kind"] == "result", result
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == int(allowed)
    assert callback.call_count == int(allowed)
    assert all(registry.get_entry(name) is old for name, old in previous.items())


@pytest.mark.parametrize("tool_count", [10, 30, 32, 128])
def test_native_mode_can_finish_reviewed_sequential_tools(setup, tool_count):
    root, request = setup
    (root / "scratch").mkdir()
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native",
                        "maxToolCalls": tool_count})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase),
                               requiredOutputs=["report.md"])
    chats = []

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            assert payload["action"] == "tool:write_file"
            return httpx.Response(200, json={"allowed": True})
        chats.append(payload)
        n = len(chats)
        if n <= tool_count:
            path = "exports/report.md" if n == tool_count else "scratch/note.md"
            return response(calls=[call("write_file", {"path": path, "content": f"Native note {n}"})])
        return response("Finished the review and persisted its report")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "result", result
    assert len(chats) == tool_count + 1
    assert result["evidence"]["rounds"] == tool_count + 1
    assert result["evidence"]["usage"]["totalTokens"] == 15 * (tool_count + 1)
    assert result["evidence"]["phase"]["successfulToolCalls"] == tool_count
    assert result["artifacts"] == {"report.md": f"Native note {tool_count}"}


def test_native_mode_recovers_from_agent_selected_http_failure(setup):
    root, request = setup
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native",
                        "capabilities": ["network"], "requiredToolCalls": []})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    chats, fetches = [], []

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            return httpx.Response(200, json={"allowed": True})
        if req.url.path == "/egress/fetch":
            fetches.append(payload["url"])
            return httpx.Response(200, json={**FETCH_PAGE, "status": 404 if len(fetches) == 1 else 200})
        chats.append(payload)
        schema = next(t["function"] for t in payload["tools"] if t["function"]["name"] == "http_fetch")
        assert "enum" not in schema["parameters"]["properties"]["url"]
        return response(calls=[call("http_fetch", {"method": "GET", "url": f"https://example.com/chosen-{len(chats)}"})]) if len(chats) < 3 else response("Used the second source after the first returned 404")

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "result", result
    assert fetches == ["https://example.com/chosen-1", "https://example.com/chosen-2"]
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 2
    assert result["evidence"]["phase"]["successfulToolCalls"] == 1


@pytest.mark.parametrize("outcome,allowed", [({"status": "queued", "update_id": "update-1"}, True),
    ({"status": "completed", "update_id": "update-1"}, True), ({"error": "Scope refused"}, True),
    ({"status": "completed", "update_id": "update-1"}, False)])
@pytest.mark.parametrize("scope_args", [{"scope": "reviewed_scope"}, {}, {"scope": None}])
def test_native_memory_only_dispatch_preserves_result_and_registration(setup, monkeypatch, outcome, allowed, scope_args):
    from tools.registry import registry
    from kars_runtime_hermes.plugin import router_client

    root, request = setup
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native",
                        "capabilities": ["memory"]})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    monkeypatch.delenv("KARS_PROVIDER", raising=False)
    monkeypatch.delenv("FOUNDRY_PROJECT_ENDPOINT", raising=False)
    previous = registry.get_entry("foundry_memory")
    chats, calls, reports = [], [], []
    args = {"operation": "update", "text": "Actual reviewed memory", **scope_args}
    forwarded = {key: value for key, value in args.items() if key != "scope" or value is not None}

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            assert payload["action"] == "tool:foundry_memory"
            assert payload["context"]["arguments"] == args
            return httpx.Response(200, json={"allowed": allowed})
        if req.url.path == "/telemetry/tool":
            reports.append(payload)
            return httpx.Response(200, json={"recorded": True, "source": "harness-reported",
                                            "enforcement_changed": False})
        if req.url.path == "/platform/mcp":
            calls.append(payload)
            assert payload["params"] == {"name": "foundry.memory", "arguments": forwarded}
            return httpx.Response(200, json={"jsonrpc": "2.0", "id": 1, "result": {
                "content": [{"type": "text", "text": json.dumps(outcome)}], "isError": False}})
        assert req.url.path == "/v1/chat/completions"
        chats.append(payload)
        assert {t["function"]["name"] for t in payload.get("tools", [])} == {"kars_access_request", "foundry_memory"}
        schema = next(t["function"]["parameters"] for t in payload["tools"] if t["function"]["name"] == "foundry_memory")
        assert schema["properties"]["operation"]["enum"] == ["search", "update"]
        assert schema["properties"]["scope"]["type"] == ["string", "null"]
        prompt = "\n".join(str(m.get("content", "")) for m in payload["messages"])
        assert "Queued updates are pending, not completed persistence" in prompt
        assert "uncontrolled local memory" in prompt
        if len(chats) == 1:
            result = response(calls=[call("foundry_memory", args)])
        else:
            tool_result = next(m["content"] for m in payload["messages"] if m.get("tool_call_id") == "call-local")
            if allowed:
                assert json.loads(tool_result) == outcome
            else:
                assert "authorization refused" in json.loads(tool_result)["error"]
            result = response("Reported the returned memory outcome without replaying it.")
        result.headers.update({"x-kars-telemetry-scope": "memory-scope", "x-kars-telemetry-round": str(len(chats))})
        return result

    transport = httpx.MockTransport(provider)
    with httpx.Client(base_url=router_client.ROUTER_BASE_URL, transport=transport, trust_env=False) as client:
        monkeypatch.setattr(router_client, "_client", lambda: client)
        result = native.execute(request, root, lambda _: None, transport=transport)
    assert result["kind"] == "result", result
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == int(allowed and "error" not in outcome)
    assert len(calls) == int(allowed) and len(chats) == 2
    assert [r["ok"] for r in reports] == (["error" not in outcome] if allowed else [])
    assert registry.get_entry("foundry_memory") is previous


@pytest.mark.parametrize("policy_allowed,tool_error", [(True, False), (True, True), (False, False)])
@pytest.mark.parametrize("list_args", [{}, {"server": ""}, {"server": "everything"}])
def test_native_mcp_reuses_router_bridge_and_reports_outcomes(
        setup, monkeypatch, policy_allowed, tool_error, list_args):
    from kars_runtime_hermes.plugin import mcp_bridge, router_client

    root, request = setup
    phase = wire.phase({**request["contract"]["reviewedPhase"], "executionMode": "native",
                        "capabilities": ["network"], "requiredToolCalls": []})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    chats, policies, requests, reports = [], [], [], []
    mcp_bridge._reset_for_tests()

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            policies.append(payload)
            return httpx.Response(200, json={"allowed": policy_allowed})
        if req.url.path == "/telemetry/tool":
            reports.append(payload)
            return httpx.Response(200, json={"recorded": True, "source": "harness-reported", "enforcement_changed": False})
        if req.url.path == "/mcp":
            expected_scope = ("everything" if payload["method"] == "tools/call"
                              else list_args.get("server") or None)
            assert req.headers.get("x-kars-mcp-server") == expected_scope
            if payload["method"] == "tools/list":
                assert payload["params"] == {}
            requests.append(payload)
            method = payload["method"]
            if method == "notifications/initialized":
                return httpx.Response(202)
            result = ({"protocolVersion": "2025-06-18", "capabilities": {}} if method == "initialize"
                      else {"tools": [{"name": "everything.echo"}]} if method == "tools/list"
                      else {"content": [{"type": "text", "text": "Actual MCP result"}], "isError": tool_error})
            return httpx.Response(200, json={"jsonrpc": "2.0", "id": payload["id"], "result": result},
                                  headers={"mcp-session-id": "native-mcp"})
        assert req.url.path == "/v1/chat/completions"
        chats.append(payload)
        names = {tool["function"]["name"] for tool in payload.get("tools", [])}
        assert {"kars_mcp_list", "kars_mcp_call"} <= names
        if len(chats) == 1:
            result = response(calls=[call("kars_mcp_list", list_args)])
        elif len(chats) == 2:
            result = response(calls=[{**call("kars_mcp_call", {"name": "everything.echo", "arguments": {"text": "Reviewed input"},
                                                            "server": "everything"}), "id": "call-mcp"}])
        else:
            result = response("Handled the actual MCP result without retries")
        result.headers.update({"x-kars-telemetry-scope": "native-mcp-scope", "x-kars-telemetry-round": str(len(chats))})
        return result

    transport = httpx.MockTransport(provider)
    with httpx.Client(base_url=router_client.ROUTER_BASE_URL, transport=transport, trust_env=False) as client:
        monkeypatch.setattr(router_client, "_client", lambda: client)
        result = native.execute(request, root, lambda _: None, transport=transport)
    mcp_bridge._reset_for_tests()
    assert result["kind"] == "result", result
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 2
    expected_successes = (1 if tool_error else 2) if policy_allowed else 0
    assert result["evidence"]["phase"]["successfulToolCalls"] == expected_successes
    assert [p["action"] for p in policies] == ["tool:kars_mcp_list", "tool:kars_mcp_call"]
    if policy_allowed:
        assert [p["method"] for p in requests] == ["initialize", "notifications/initialized", "tools/list", "tools/call"]
        assert [p["ok"] for p in reports] == [True, not tool_error]
    else:
        assert requests == reports == []


def reviewed_fetch_run(setup, completions, *, envelope=None, allowed=True, outer_status=200):
    root, request = setup
    phase = request["contract"]["reviewedPhase"]
    if "network" not in phase["capabilities"]:
        phase["capabilities"].append("network")
        phase["requiredToolCalls"] = [dict(name="http_fetch", arguments=FETCH_ARGS.copy())]
    phase = wire.phase(phase)
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    chats, fetches, policies = [], [], []

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            policies.append(payload)
            return httpx.Response(200, json={"allowed": allowed})
        if req.url.path == "/egress/fetch":
            fetches.append(payload)
            assert req.extensions["timeout"] == dict(connect=5, read=35, write=10, pool=5)
            return httpx.Response(outer_status, json=FETCH_PAGE if envelope is None else envelope)
        assert req.url.path == "/v1/chat/completions"
        chats.append(payload)
        return completions.pop(0)

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    return result, chats, fetches, policies


def test_native_reviewed_fetch_uses_real_governed_tool_and_exact_authority(setup):
    result, chats, fetches, policies = reviewed_fetch_run(setup, [
        response(calls=[call("http_fetch", FETCH_ARGS)]), response("Used published terms")])
    assert result["kind"] == "result", result
    assert result["evidence"]["phase"]["completedRequiredToolCalls"] == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == 1
    assert len(chats) == 2 and result["evidence"]["rounds"] == 2
    assert chats[0]["tool_choice"]["function"]["name"] == "http_fetch"
    assert chats[0]["tools"][0]["function"]["parameters"]["properties"]["url"]["enum"] == [FETCH_ARGS["url"]]
    assert fetches == [{**FETCH_ARGS, "headers": {}, "body": ""}]
    assert policies == [dict(action="tool:http_fetch", context=dict(
        tool_call_id="call-local", tool="http_fetch", arguments=FETCH_ARGS))]
    assert "Official published terms" in json.dumps(chats[-1]["messages"])


@pytest.mark.parametrize("native_mode,upstream_status", [(False, 200), (True, 200), (True, 403)])
@pytest.mark.parametrize("origin_wait", [False, True])
@pytest.mark.parametrize("github_repo", [False, True])
def test_native_fetch_correlates_policy_egress_and_result(setup, native_mode, upstream_status, origin_wait, github_repo):
    root, request = setup
    phase = wire.phase({**request["contract"]["reviewedPhase"], "capabilities": ["network"],
                        **({"executionMode": "native"} if native_mode else {}),
                        "requiredToolCalls": [] if native_mode else [dict(name="http_fetch", arguments=FETCH_ARGS)]})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    args = {"method": "GET", "url": "https://example.net", "wait_for_approval_ms": 0} if native_mode and origin_wait else dict(FETCH_ARGS)
    if native_mode and github_repo:
        args["url"] = "https://api.github.com/repos/example/private-repo"
    chats, fetches, policies, reports = [], [], [], []

    def provider(req):
        payload = json.loads(req.content)
        if req.url.path == "/agt/evaluate":
            policies.append(payload)
            return httpx.Response(200, json={"allowed": True})
        if req.url.path == "/egress/fetch":
            fetches.append(payload)
            return httpx.Response(200, json={**FETCH_PAGE, "status": upstream_status,
                                             "observed_at": "2026-10-31T08:30:00.123Z"})
        if req.url.path == "/telemetry/tool":
            reports.append(payload)
            return httpx.Response(200, json={"recorded": True, "source": "harness-reported",
                                            "enforcement_changed": False})
        assert req.url.path == "/v1/chat/completions"
        chats.append(payload)
        result = response(calls=[call("http_fetch", args)]) if len(chats) == 1 else response("Source evaluated")
        result.headers.update({"x-kars-telemetry-scope": "native-fetch-scope",
                               "x-kars-telemetry-round": str(len(chats))})
        return result

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(provider))
    assert result["kind"] == "result", result
    assert len(chats) == 2
    tool_result = next(message for message in chats[1]["messages"] if message["role"] == "tool")
    assert json.loads(tool_result["content"])["observed_at"] == "2026-10-31T08:30:00.123Z"
    assert policies == [dict(action="tool:http_fetch", context=dict(
        scope_id="native-fetch-scope", tool_call_id="call-local", tool="http_fetch", arguments=args))]
    assert fetches == [{**{k: v for k, v in args.items() if k != "wait_for_approval_ms"}, "headers": {}, "body": "",
                        "telemetry": {"scope_id": "native-fetch-scope", "call_id": "call-local"}}]
    assert len(reports) == 1
    assert reports[0]["scope_id"] == "native-fetch-scope" and reports[0]["call_id"] == "call-local"
    assert reports[0]["ok"] is (upstream_status == 200)
    assert result["evidence"]["phase"]["successfulToolCalls"] == int(upstream_status == 200)
    prompt = "\n".join(str(message.get("content", "")) for message in chats[0]["messages"])
    schema = next(tool["function"] for tool in chats[0]["tools"] if tool["function"]["name"] == "http_fetch")
    if native_mode:
        assert "GitHub App connection (not MCP)" in schema["description"]
        assert "use http_fetch with method GET" in prompt
        assert "native GitHub App read adapter, not an MCP server or a separate tool" in prompt
        assert "This guidance grants no access" in prompt
        assert "/branches/{branch} and /git/ref/heads/{branch} are unsupported" in prompt
        assert "preserve the status and limitation" in prompt
        assert not any(tool["function"]["name"] == "github_pr" for tool in chats[0]["tools"])
    else:
        assert "native GitHub App read adapter" not in prompt
        assert "GitHub App connection (not MCP)" not in schema["description"]


@pytest.mark.parametrize("envelope", [dict(FETCH_PAGE, status=302), dict(FETCH_PAGE, status=403),
    dict(FETCH_PAGE, truncated=True), dict(FETCH_PAGE, body=""), dict(FETCH_PAGE, body="x" * 65537),
    dict(FETCH_PAGE, body="bad\ufffd"), dict(FETCH_PAGE, headers=[]),
    dict(FETCH_PAGE, headers={"oversized": "x" * (512 * 1024)}), {"error": "denied"}, "malformed envelope"])
def test_native_reviewed_fetch_never_credits_failed_envelopes(setup, envelope):
    result, _, fetches, _ = reviewed_fetch_run(setup, [
        response(calls=[call("http_fetch", FETCH_ARGS)]), response()], envelope=envelope)
    assert result["kind"] == "error", result
    assert len(fetches) == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == 0
    assert result["evidence"]["phase"]["completedRequiredToolCalls"] == 0


@pytest.mark.parametrize("allowed,outer_status", [(False, 200), (None, 200), ("true", 200), (True, 403)])
def test_native_reviewed_fetch_refusal_cannot_satisfy_requirement(setup, allowed, outer_status):
    result, _, fetches, _ = reviewed_fetch_run(setup, [
        response(calls=[call("http_fetch", FETCH_ARGS)]), response()], allowed=allowed, outer_status=outer_status)
    assert result["kind"] == "error", result
    assert len(fetches) == (1 if allowed is True else 0)
    assert result["evidence"]["phase"]["completedRequiredToolCalls"] == 0
    assert result["evidence"]["phase"]["successfulToolCalls"] == 0


@pytest.mark.parametrize("args", [dict(FETCH_ARGS, headers={}), dict(FETCH_ARGS, method="POST"),
    dict(FETCH_ARGS, url="https://www.example.com/unreviewed")])
def test_native_reviewed_fetch_checks_arguments_before_policy_and_network(setup, args):
    result, _, fetches, policies = reviewed_fetch_run(setup, [
        response(calls=[call("http_fetch", args)]), response()])
    assert result["kind"] == "error", result
    assert fetches == policies == []
    assert result["evidence"]["phase"]["completedRequiredToolCalls"] == 0


def test_native_reviewed_fetch_orders_inputs_fetch_and_exact_exports(setup, input_artifact):
    root, request = setup
    request["inputArtifacts"] = [input_artifact("facts.md", "Immutable prior facts")]
    (root / "inputs/facts.md").write_text("Immutable prior facts")
    request["contract"]["requiredOutputs"] = ["report.md"]
    result, chats, fetches, _ = reviewed_fetch_run(setup, [
        response(calls=[call("kars_mission_read", {"path": "inputs/facts.md"})]),
        response(calls=[call("http_fetch", FETCH_ARGS)]),
        response(calls=[call("kars_mission_write", {"path": "exports/report.md", "content": "Evidence-based report"})]),
        response("Exported report.md")])
    assert result["kind"] == "result", result
    assert chats[0]["tool_choice"]["function"]["name"] == "kars_mission_read"
    assert chats[1]["tool_choice"]["function"]["name"] == "http_fetch"
    assert result["artifacts"] == {"report.md": "Evidence-based report"}
    assert result["evidence"]["phase"]["successfulToolCalls"] == 3
    assert len(fetches) == 1
    assert (root / "inputs/facts.md").read_text() == "Immutable prior facts"


def test_native_required_fetches_reserve_export_slots_before_inference(setup):
    _, request = setup
    request["contract"]["requiredOutputs"] = ["report.md"]
    request["contract"]["reviewedPhase"]["maxToolCalls"] = 1
    result, chats, fetches, policies = reviewed_fetch_run(setup, [])
    assert result["kind"] == "error" and "Insufficient reviewed tool calls" in result["error"]
    assert chats == fetches == policies == []


def test_native_repeated_fetch_cannot_replace_a_different_required_url(setup):
    _, request = setup
    phase = request["contract"]["reviewedPhase"]
    phase["capabilities"].append("network")
    phase["requiredToolCalls"] = [dict(name="http_fetch", arguments=args) for args in
                                  (FETCH_ARGS, dict(FETCH_ARGS, url="https://www.example.com/second"))]
    result, _, fetches, _ = reviewed_fetch_run(setup, [
        response(calls=[call("http_fetch", FETCH_ARGS)]), response(calls=[call("http_fetch", FETCH_ARGS)]), response()])
    assert result["kind"] == "error", result
    assert len(fetches) == 2
    assert result["evidence"]["phase"]["successfulToolCalls"] == 2
    assert result["evidence"]["phase"]["completedRequiredToolCalls"] == 1


@pytest.mark.parametrize("allowed", [False, None, "true"])
def test_native_filesystem_authorization_is_fail_closed_without_activity_negotiation(setup, allowed):
    root, request = setup
    paths, messages = [], []

    def transport(req):
        paths.append(req.url.path)
        if req.url.path == "/agt/evaluate":
            assert json.loads(req.content) == {
                "action": "tool:kars_mission_write", "context": {"tool_call_id": "call-local"}}
            return httpx.Response(200, json={"allowed": allowed})
        assert req.url.path == "/v1/chat/completions"
        messages.append(json.loads(req.content)["messages"])
        return response(calls=[call("kars_mission_write", {"path": "exports/report.md", "content": "Denied"})]
                        if len(messages) == 1 else None)

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(transport))
    assert result["kind"] == "result", result
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == 0
    assert not (root / "exports/report.md").exists()
    assert "Filesystem tool authorization refused" in json.dumps(messages[-1])
    assert paths == ["/v1/chat/completions", "/agt/evaluate", "/v1/chat/completions"]


@pytest.mark.parametrize("caller_timeout", [1, 900, None])
def test_measured_transport_pins_inference_timeout_over_native_options(caller_timeout):
    seen = []

    def provider(request):
        seen.append(request.extensions["timeout"])
        return response()

    measurements = native.Measurements("mission-model", None, lambda _: None)
    boundary = native.MeasuredTransport(measurements, httpx.MockTransport(provider), set())
    with httpx.Client(transport=boundary, timeout=caller_timeout) as client:
        result = client.post(native.ROUTER + "/chat/completions", json={"model": "mission-model"})
    assert result.status_code == 200
    assert seen == [{"connect": 5, "read": 630, "write": 10, "pool": 5}]
    assert measurements.rounds == 1
    assert measurements.usage["totalTokens"] == 15


@pytest.mark.parametrize("status", [401, 429, 503])
@pytest.mark.parametrize("content,finish", [(None, "stop"), ("private answer", "private finish")])
def test_rejection_retains_safe_completion_shape_not_provider_content(status, content, finish):
    previous = response(content=content).json()
    previous["choices"][0]["finish_reason"] = finish
    replies = [httpx.Response(200, json=previous), httpx.Response(status, json={"error": "private rejection"})]
    events = []
    measurements = native.Measurements("mission-model", None, events.append)
    boundary = native.MeasuredTransport(measurements, httpx.MockTransport(lambda _: replies.pop(0)), set())
    with httpx.Client(transport=boundary) as client:
        client.post(native.ROUTER + "/chat/completions", json={"model": "mission-model"})
        with pytest.raises(RuntimeError, match=f"Inference request rejected \\(HTTP {status}\\)"):
            client.post(native.ROUTER + "/chat/completions", json={"model": "mission-model"})
    assert measurements.rounds == 2 and measurements.usage is None
    assert "private" not in measurements.failure
    assert f"finish={'stop' if finish == 'stop' else 'other'}" in measurements.failure
    assert f"textChars={len(content or '')}, toolCalls=0, refusal=False" in measurements.failure
    assert len(measurements.failure) < 512
    assert events[-1]["evidence"]["usage"] is None


def test_real_native_conversation_usage(setup):
    result, seen, events = run(setup, [response()])
    assert result["kind"] == "result", result
    assert result["output"] == "A real native answer"
    assert result["evidence"]["usage"]["totalTokens"] == 15
    assert len(seen) == 1
    assert events[0]["evidence"]["rounds"] == 1


@pytest.mark.skipif(sys.platform != "linux" or os.getuid() != 1000
                    or os.environ.get("KARS_PYTHON_SANDBOX_TESTS") != "1",
                    reason="Requires the actual Linux Task sandbox")
@pytest.mark.parametrize("exit_code", [0, 7])
def test_native_python_conversation_executes_and_retains_actual_output(setup, exit_code):
    root, request = setup
    request.update(taskUid="native-python-task", runNonce=f"native-python-{exit_code}",
                   activityFormat=wire.ACTIVITY, artifactFormat="text-v1")
    phase = wire.phase({"name": "execute", "objective": "Execute Python and retain the actual output",
                        "executionMode": "native", "maxToolCalls": 4,
                        "capabilities": ["filesystem-read", "filesystem-write", "network", "python-execute"]})
    request["contract"].update(reviewedPhase=phase, phaseDigest=wire.digest(phase))
    source = f"print('Hermes executed this application')\nraise SystemExit({exit_code})\n"
    actions = [("write_file", {"path": "scratch/app.py", "content": source}),
               ("python_execute", {"entrypoint": "app.py", "files": [{"path": "scratch/app.py", "name": "app.py",
                    "sha256": "sha256:" + hashlib.sha256(source.encode()).hexdigest()}]})]
    reports, policies = [], []
    rounds = 0

    def transport(req):
        nonlocal rounds
        if req.url.path == "/v1/chat/completions":
            rounds += 1
            tool = call(*actions[rounds - 1]) if rounds <= len(actions) else None
            if tool:
                tool["id"] = f"python-call-{rounds}"
            result = response(calls=[tool] if tool else None)
            result.headers.update({"x-kars-telemetry-scope": "python-scope",
                                   "x-kars-telemetry-round": str(rounds)})
            return result
        if req.url.path == "/agt/evaluate":
            policies.append(json.loads(req.content))
            return httpx.Response(200, json={"allowed": True})
        if req.url.path == "/telemetry/tool":
            reports.append(json.loads(req.content))
            return httpx.Response(200, json={"recorded": True, "source": "harness-reported",
                                            "enforcement_changed": False})
        assert req.url.path == "/telemetry/trace"
        return httpx.Response(200, json=dict(scope_id="python-scope", rounds=list(range(1, rounds + 1)),
                              coverage="returned-responses-only", durable=False, missing_rounds=[],
                              dropped_events=0, truncated=False, events=[], cursor=0, high_water=0, has_more=False))

    result = native.execute(request, root, lambda _: None, transport=httpx.MockTransport(transport))
    assert result["kind"] == "result", result
    assert "kars-python-executions.json" in result["artifacts"], {"result": result, "reports": reports, "policies": policies}
    receipt, = json.loads(result["artifacts"]["kars-python-executions.json"])["runs"]
    assert receipt["exit_code"] == exit_code and receipt["ok"] == (exit_code == 0)
    assert receipt["stdout"]["text"] == "Hermes executed this application\n"
    assert receipt["descendants_cleaned"] is True
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 2
    assert result["evidence"]["phase"]["successfulToolCalls"] == 1 + int(exit_code == 0)
    assert result["evidence"]["usage"]["totalTokens"] == 45
    assert [p["action"] for p in policies] == ["tool:write_file", "tool:python_execute"]
    assert policies[-1]["context"] == {"tool_call_id": "python-call-2", "scope_id": "python-scope",
                                       "tool": "python_execute", "arguments": actions[1][1]}
    assert [r["ok"] for r in reports] == [True, exit_code == 0]
    assert reports[-1]["result_summary"]["kind"] == "python_execution"


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
            cursor=1, high_water=1, has_more=False,
            events=[dict(kind="round", scope_id="native-scope", round=1, seq=1, source="router-observed",
                         model="mission-model", http_status=200)]))

    result = native.execute(request, root, events.append, transport=httpx.MockTransport(transport))
    assert result["kind"] == "result", result
    assert result["evidence"]["rounds"] == 1 and result["evidence"]["usage"]["totalTokens"] == 15
    activity = json.loads(events[0]["routerObservations"])
    assert activity["state"] == "observed" and activity["rounds"] == [1]
    assert json.loads(result["artifacts"]["kars-router-observations.json"]) == activity
    assert paths == ["/v1/chat/completions", "/telemetry/trace", "/telemetry/trace"]


@pytest.mark.parametrize("recorder_available", [True, False])
def test_native_tool_callbacks_report_results_before_next_model_call(setup, recorder_available):
    root, request = setup
    request.update(activityFormat=wire.ACTIVITY, artifactFormat="text-v1")
    request["contract"]["requiredOutputs"] = ["report.md"]
    content = "Actual native export\r\n✓"
    actions = [("kars_mission_write", {"path": "exports/report.md", "content": content}),
               ("kars_mission_read", {"path": "exports/report.md"}),
               ("kars_mission_read", {"path": "inputs/missing.md"})]
    progress, reports, recorded = [], [], []
    authorized = set()
    rounds = 0

    def transport(req):
        nonlocal rounds
        if req.url.path == "/v1/chat/completions":
            assert len(reports) == rounds
            rounds += 1
            tool = call(*actions[rounds - 1]) if rounds <= len(actions) else None
            if tool:
                tool["id"] = f"native-call-{rounds}"
                recorded.append(dict(kind="tool_proposed", scope_id="native-tools", round=rounds,
                                     seq=len(recorded) + 1, source="model-proposed",
                                     call_id=tool["id"], name=actions[rounds - 1][0]))
            result = response(calls=[tool] if tool else None)
            result.headers.update({"x-kars-telemetry-scope": "native-tools", "x-kars-telemetry-round": str(rounds)})
            return result
        if req.url.path == "/agt/evaluate":
            decision = json.loads(req.content)
            assert decision == {"action": "tool:" + actions[rounds - 1][0],
                                "context": {"scope_id": "native-tools", "tool_call_id": f"native-call-{rounds}"}}
            assert not authorized
            authorized.add(("native-tools", f"native-call-{rounds}"))
            return httpx.Response(200, json={"allowed": True})
        if req.url.path == "/telemetry/tool":
            report = json.loads(req.content)
            assert (report["scope_id"], report["call_id"]) in authorized
            authorized.remove((report["scope_id"], report["call_id"]))
            reports.append(report)
            assert report["scope_id"] == "native-tools" and report["call_id"] == f"native-call-{rounds}"
            assert content not in req.content.decode()
            if not recorder_available:
                return httpx.Response(503)
            recorded.append(dict(kind="tool_result", scope_id="native-tools", round=rounds,
                                 seq=len(recorded) + 1, source="harness-reported",
                                 call_id=report["call_id"], name=actions[rounds - 1][0], ok=report["ok"],
                                 ms=report["latency_ms"], result_summary=report["result_summary"]))
            return httpx.Response(200, json={"recorded": True, "source": "harness-reported", "enforcement_changed": False})
        assert req.url.path == "/telemetry/trace"
        return httpx.Response(200, json=dict(scope_id="native-tools", rounds=list(range(1, rounds + 1)),
                              coverage="returned-responses-only", durable=False, missing_rounds=[],
                              dropped_events=0, truncated=False, events=list(recorded),
                              cursor=len(recorded), high_water=len(recorded), has_more=False))

    result = native.execute(request, root, progress.append, transport=httpx.MockTransport(transport))
    assert result["kind"] == "result", result
    assert result["artifacts"]["report.md"] == content
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 3
    assert result["evidence"]["phase"]["successfulToolCalls"] == 2
    assert result["evidence"]["usage"]["totalTokens"] == 60
    assert [report["ok"] for report in reports] == [True, True, False]
    size = len(content.encode())
    assert [report["result_summary"] for report in reports] == [
        {"kind": "file_write", "bytes": size, "artifact_name": "report.md"},
        {"kind": "file_read", "bytes": size, "returned_bytes": size, "truncated": False},
        {"kind": "failure", "reason": "filesystem_operation_failed"}]
    final = json.loads(result["artifacts"]["kars-router-observations.json"])
    assert final["trace"]["events"] == recorded
    assert len([event for event in recorded if event["kind"] == "tool_result"]) == (3 if recorder_available else 0)
    if recorder_available:
        assert any(event["evidence"]["rounds"] == 1
                   and any(item["kind"] == "tool_result"
                           for item in json.loads(event["routerObservations"])["trace"]["events"])
                   for event in progress)


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
    assert any("requires 1 successful authorized tool calls; 6 attempts remain" in message.get("content", "")
               for message in seen[2]["messages"] if message["role"] == "system")
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


@pytest.mark.parametrize("handoff_first", [True, False])
def test_native_two_call_handoff_reserves_exports_and_finishes_without_tools(setup, handoff_first):
    _, req = setup
    req["contract"]["requiredOutputs"] = ["handoff.md"]
    req["contract"]["reviewedPhase"].update(minToolCalls=1, maxToolCalls=2)
    first = "handoff.md" if handoff_first else "coverage-brief.txt"
    second = call("kars_mission_read", {"path": "exports/handoff.md"}) if handoff_first else call(
        "kars_mission_write", {"path": "exports/handoff.md", "content": "Complete phase handoff"})
    result, seen, _ = run(setup, [
        response(calls=[call("kars_mission_write", {"path": "exports/" + first,
                                                   "content": "Complete phase handoff"})]),
        response(calls=[second]), response("Phase handoff complete")])
    assert result["kind"] == "result", result
    assert result["artifacts"]["handoff.md"] == "Complete phase handoff"
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 2
    assert result["evidence"]["phase"]["successfulToolCalls"] == 2
    assert result["evidence"]["usage"]["totalTokens"] == 45
    assert all(request["parallel_tool_calls"] is False for request in seen[:2])
    assert "Only 1 reviewed tool attempts remain" in seen[1]["messages"][-1]["content"]
    if not handoff_first:
        assert seen[1]["tool_choice"]["function"]["name"] == "kars_mission_write"
        assert seen[1]["tools"][0]["function"]["parameters"]["properties"]["path"]["enum"] == [
            "exports/handoff.md"]
    assert not {"tools", "parallel_tool_calls", "tool_choice"} & seen[2].keys()
    assert "No further tool calls are permitted" in seen[2]["messages"][-1]["content"]


def test_native_empty_completion_retry_keeps_tool_selection_closed(setup):
    _, req = setup
    req["contract"]["requiredOutputs"] = ["handoff.md"]
    req["contract"]["reviewedPhase"].update(minToolCalls=1, maxToolCalls=1)
    result, seen, _ = run(setup, [
        response(calls=[call("kars_mission_write", {"path": "exports/handoff.md", "content": "Verified handoff"})]),
        response(content=None), response("The verified handoff is ready")])
    assert result["kind"] == "result", result
    assert result["artifacts"]["handoff.md"] == "Verified handoff"
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == 1
    assert len(seen) == 3 and result["evidence"]["usage"]["totalTokens"] == 45
    for request in seen[1:]:
        assert not {"tools", "parallel_tool_calls", "tool_choice"} & request.keys()
        assert "No further tool calls are permitted" in request["messages"][-1]["content"]


def test_native_oversized_remaining_batch_still_executes_nothing(setup):
    root, req = setup
    req["contract"]["requiredOutputs"] = ["handoff.md"]
    req["contract"]["reviewedPhase"].update(minToolCalls=1, maxToolCalls=2)
    result, seen, _ = run(setup, [
        response(calls=[call("kars_mission_write", {"path": "exports/handoff.md", "content": "Original handoff"})]),
        response(calls=[call("kars_mission_write", {"path": "exports/handoff.md", "content": "Overwrite"}),
                        call("kars_mission_write", {"path": "exports/extra.md", "content": "Extra"})])])
    assert result["kind"] == "error" and result["error"] == "Tool invocation limit reached"
    assert len(seen) == 2 and seen[1]["parallel_tool_calls"] is False
    assert result["evidence"]["phase"]["attemptedToolCalls"] == 1
    assert result["evidence"]["phase"]["successfulToolCalls"] == 1
    assert (root / "exports/handoff.md").read_text() == "Original handoff"
    assert not (root / "exports/extra.md").exists()


def test_native_missing_export_at_exhaustion_cannot_be_finalized(setup):
    _, req = setup
    req["contract"]["requiredOutputs"] = ["handoff.md"]
    req["contract"]["reviewedPhase"].update(minToolCalls=1, maxToolCalls=1)
    result, seen, _ = run(setup, [response(calls=[call(
        "kars_mission_write", {"path": "exports/wrong.md", "content": "Not a required handoff"})])])
    assert result["kind"] == "error" and "Insufficient remaining reviewed tool calls" in result["error"]
    assert len(seen) == 1 and result["evidence"]["rounds"] == 1


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
    assert not {"tools", "parallel_tool_calls", "tool_choice"} & seen[-1].keys()


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


def test_native_read_timeout_does_not_retry_or_invent_usage(setup):
    def timed_out(_):
        raise httpx.ReadTimeout("Router response deadline elapsed")

    result, seen, _ = run(setup, timed_out)
    assert result["kind"] == "error"
    assert result["evidence"]["usage"] is None
    assert len(seen) == 1


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
    assert not {"tools", "parallel_tool_calls", "tool_choice"} & seen[-1].keys()


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
