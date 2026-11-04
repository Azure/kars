# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json
import sys
from types import SimpleNamespace

import httpx
import pytest

from kars_runtime_hermes.plugin import mission_native as native, mission_protocol as wire, native_github_pr
from kars_runtime_hermes.plugin.mission_native_tools import NativeTools
from kars_runtime_hermes.plugin.mission_observations import ReturnedRouterResponses
from test_mission_memory import Registry

ARGS = {"operation": "create", "repository": "owner/repo", "path": "README.md", "expected_base": "a" * 40,
        "content": "Reviewed documentation", "title": "Docs", "body": "One file"}


@pytest.fixture
def github(tmp_path, monkeypatch):
    state = {"allowed": True, "recorded": True, "calls": [], "reports": [], "policies": [],
             "result": {"http_status": 201, "response": {"html_url": "https://github.com/owner/repo/pull/17"}}}

    def provider(request):
        payload = json.loads(request.content)
        if request.url.path == "/agt/evaluate":
            state["policies"].append(payload)
            return httpx.Response(200, json={"allowed": state["allowed"]})
        assert request.url.path == "/telemetry/tool"
        state["reports"].append(payload)
        return httpx.Response(200, json={"recorded": state["recorded"], "source": "harness-reported", "enforcement_changed": False})

    def invoke(identity, args, cancelled):
        assert identity == ["task", "run", "assignment"] and not cancelled()
        state["calls"].append(args)
        if state.get("cancel"):
            tools.closed = True
        return state["result"]

    observations = ReturnedRouterResponses(httpx.MockTransport(provider))
    observations.scope = "scope-1"
    phase = wire.phase({"name": "docs", "objective": "Produce the reviewed documentation PR", "executionMode": "native",
                        "capabilities": ["github-pr"], "maxToolCalls": 8})
    measurements = native.Measurements("test-model", phase, lambda _: None, observations)
    tools = NativeTools(tmp_path, measurements)
    tools.github_identity = ["task", "run", "assignment"]
    monkeypatch.setattr(native_github_pr, "invoke", invoke)
    return tools, measurements, state


def call(github, args=ARGS):
    tools, m, _ = github
    m.attempted += 1
    tools.start("github-" + str(m.attempted), "github_pr", args)
    return json.loads(tools.invoke_github(args))


def test_governed_publication_preserves_observed_response_and_receipt(github):
    tools, m, state = github
    assert call(github) == state["result"]
    assert m.successful == 1 and m.failure is None
    assert state["policies"] == [{"action": "tool:github_pr", "context": {
        "scope_id": "scope-1", "tool_call_id": "github-1", "tool": "github_pr", "arguments": ARGS}}]
    assert state["calls"] == [ARGS] and [r["ok"] for r in state["reports"]] == [True]
    assert "error" in json.loads(tools.invoke_github(ARGS))
    assert state["calls"] == [ARGS]  # No replay without a new observed invocation.


@pytest.mark.parametrize("failure", ["readonly", "legacy", "denied", "invalid", "unobserved", "scope", "closed", "identity"])
def test_missing_authority_cannot_dispatch_helper(github, failure):
    tools, m, state = github
    args = ARGS
    if failure == "readonly":
        m.phase["capabilities"] = ["filesystem-read", "filesystem-write", "network"]
    if failure == "legacy":
        m.phase.pop("executionMode")
    if failure == "denied":
        state["allowed"] = False
    if failure == "invalid":
        args = {**ARGS, "path": ".github/workflows/ci.yml"}
    if failure == "scope":
        m.observations.scope = None
    if failure == "closed":
        tools.closed = True
    if failure == "identity":
        tools.github_identity = None
    result = json.loads(tools.invoke_github(args)) if failure == "unobserved" else call(github, args)
    assert "error" in result and not state["calls"] and m.successful == 0


def test_ambiguous_response_is_failed_evidence_not_a_success(github):
    _, m, state = github
    state["result"] = {"error": "Response lost", "ambiguous": True, "replay_forbidden": True, "branch": "kars/reviewed/id"}
    assert call(github) == state["result"]
    assert m.successful == 0 and [r["ok"] for r in state["reports"]] == [False]
    assert len(state["calls"]) == 1


def test_missing_receipt_halts_before_a_subsequent_mutation(github):
    _, m, state = github
    state["recorded"] = False
    call(github)
    assert "do not replay" in m.failure
    assert "error" in call(github) and len(state["calls"]) == 1


def test_cancelled_helper_result_cannot_be_counted_as_success(github):
    _, m, state = github
    state["cancel"] = True
    assert "cancelled" in call(github)["error"]
    assert m.successful == 0 and [r["ok"] for r in state["reports"]] == [False]


@pytest.mark.parametrize("granted", [False, True])
@pytest.mark.parametrize("existing", [False, True])
def test_registry_only_exposes_reviewed_github_and_restores_prior_handler(github, monkeypatch, granted, existing):
    tools, m, state = github
    if not granted:
        m.phase["capabilities"] = []
    registry = Registry()
    if existing:
        registry.register(name="github_pr", toolset="unrelated", schema={"name": "github_pr"},
                          handler=lambda *_: pytest.fail("Original handler cannot be used by native assignment"))
    previous = registry.get_entry("github_pr")
    monkeypatch.setitem(sys.modules, "tools", SimpleNamespace(file_tools=object()))
    monkeypatch.setattr(tools.spill, "install", lambda: None)
    monkeypatch.setattr(tools.spill, "close", lambda: None)
    try:
        definitions = tools.install(registry)
        names = {tool["function"]["name"] for tool in definitions}
        assert ("github_pr" in names) == granted
        if granted:
            assert registry.get_entry("github_pr").schema == native_github_pr.SCHEMA
            m.attempted = 1
            tools.start("github-1", "github_pr", ARGS)
            assert json.loads(registry.get_entry("github_pr").handler(ARGS)) == state["result"]
    finally:
        tools.close()
    assert registry.get_entry("github_pr") is previous


def test_wire_digest_binds_explicit_native_github_capability():
    phase = {"name": "docs", "objective": "Produce the reviewed documentation PR", "executionMode": "native",
             "capabilities": ["github-pr"], "maxToolCalls": 8}
    assert wire.phase(phase)["capabilities"] == ["github-pr"]
    phase.pop("executionMode")
    with pytest.raises(ValueError):
        wire.phase(phase)
