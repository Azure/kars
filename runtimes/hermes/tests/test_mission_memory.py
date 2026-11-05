# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Reviewed memory guards and real router client; no native-loop qualification implied."""

import json
import sys
from types import SimpleNamespace

import httpx
import pytest

from kars_runtime_hermes.plugin import foundry, mission_native as native, mission_protocol as wire
from kars_runtime_hermes.plugin.mission_native_tools import MEMORY_SCHEMA, NativeTools
from kars_runtime_hermes.plugin.mission_observations import ReturnedRouterResponses


SEARCH = {"operation": "search", "scope": "reviewed_scope", "query": "retained decisions", "top_k": 5}
UPDATE = {"operation": "update", "scope": "reviewed_scope", "text": "Reviewed decision"}


def memory_phase(capabilities=None):
    return wire.phase(
        dict(
            name="remember",
            objective="Reuse governed memory for reviewed decisions",
            capabilities=["memory"] if capabilities is None else capabilities,
            executionMode="native",
            maxToolCalls=8,
        )
    )


@pytest.fixture
def memory(tmp_path, monkeypatch):
    monkeypatch.setenv("KARS_PROVIDER", "azure-openai")
    monkeypatch.setenv("FOUNDRY_PROJECT_ENDPOINT", "https://foundry.example/api/projects/reviewed")
    state = {
        "allowed": True,
        "status": 200,
        "result": {"memories": []},
        "isError": False,
        "policies": [],
        "calls": [],
        "reports": [],
    }

    def provider(request):
        payload = json.loads(request.content)
        if request.url.path == "/agt/evaluate":
            state["policies"].append(payload)
            return httpx.Response(200, json={"allowed": state["allowed"]})
        if request.url.path == "/telemetry/tool":
            state["reports"].append(payload)
            return httpx.Response(
                200, json={"recorded": True, "source": "harness-reported", "enforcement_changed": False}
            )
        assert request.url.path == "/platform/mcp"
        assert request.headers["accept"] == "application/json, text/event-stream"
        state["calls"].append(payload)
        if state.get("timeout"):
            raise httpx.ReadTimeout("provider unavailable")
        return httpx.Response(
            state["status"],
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": json.dumps(state["result"])}],
                    "isError": state["isError"],
                },
            },
        )

    transport = httpx.MockTransport(provider)
    observations = ReturnedRouterResponses(transport)
    observations.capture(httpx.Headers({"x-kars-telemetry-scope": "returned-scope", "x-kars-telemetry-round": "1"}))
    measurements = native.Measurements("test-model", memory_phase(), lambda _: None, observations)
    tools = NativeTools(tmp_path, measurements)
    with httpx.Client(base_url=foundry.router_client.ROUTER_BASE_URL, transport=transport) as client:
        monkeypatch.setattr(foundry.router_client, "_client", lambda: client)
        yield tools, measurements, state


def invoke(memory, args):
    tools, measurements, _ = memory
    measurements.attempted += 1
    tools.start(f"memory-{measurements.attempted}", "foundry_memory", args)
    return tools.invoke_memory("foundry_memory", foundry._foundry_memory, args)


@pytest.mark.parametrize(
    "arguments,outcome",
    [
        (SEARCH, {"memories": [{"content": "Retained decision"}]}),
        (UPDATE, {"status": "queued", "update_id": "update-1"}),
        (UPDATE, {"status": "completed", "update_id": "update-1", "usage": {"total_tokens": 17}}),
    ],
)
def test_memory_only_reuses_platform_client_and_preserves_outcomes(memory, arguments, outcome):
    tools, measurements, state = memory
    state["result"] = outcome
    assert json.loads(invoke(memory, arguments)) == outcome
    assert measurements.phase["capabilities"] == ["memory"]
    assert measurements.attempted == measurements.successful == 1
    assert state["calls"] == [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "foundry.memory", "arguments": arguments},
        }
    ]
    assert state["policies"] == [
        {
            "action": "tool:foundry_memory",
            "context": {
                "tool_call_id": "memory-1",
                "tool": "foundry_memory",
                "arguments": arguments,
                "scope_id": "returned-scope",
            },
        }
    ]
    assert [r["ok"] for r in state["reports"]] == [True]
    assert "Reviewed decision" not in json.dumps(state["reports"])
    assert list(tools.root.iterdir()) == []  # Never replace governed storage with local files.
    assert "error" in json.loads(tools.invoke_memory("foundry_memory", foundry._foundry_memory, arguments))
    assert len(state["calls"]) == len(state["reports"]) == 1


@pytest.mark.parametrize(
    "patch",
    [
        {"operation": "delete_scope"},
        {"operation": "unknown"},
        {"scope": ""},
        {"scope": " \n"},
        {"scope": 1},
        {"text": 1},
        {"query": None},
        {"top_k": 0},
        {"top_k": -1},
        {"top_k": 1.5},
        {"top_k": True},
        {"top_k": 2**53},
        {"store_name": "another-store"},
        {"role": "system"},
        {"operation": "update"},
        {"operation": "update", "text": ""},
        {"operation": "update", "text": "  "},
    ],
)
def test_malformed_memory_is_reported_without_dispatch(memory, patch):
    _, measurements, state = memory
    assert "error" in json.loads(invoke(memory, {**SEARCH, **patch}))
    assert not state["calls"] and measurements.successful == 0
    assert [r["ok"] for r in state["reports"]] == [False]


@pytest.mark.parametrize("scope_args", [{}, {"scope": None}])
def test_bound_private_scope_is_forwarded_without_an_invented_name(memory, scope_args):
    assert MEMORY_SCHEMA["parameters"]["properties"]["scope"]["type"] == ["string", "null"]
    assert MEMORY_SCHEMA["parameters"]["required"] == ["operation"]
    args = {"operation": "search", "query": "pilot", **scope_args}
    state = memory[2]
    state["result"] = {"memories": []}
    assert json.loads(invoke(memory, args)) == state["result"]
    assert state["calls"][0]["params"]["arguments"] == {"operation": "search", "query": "pilot"}
    assert state["policies"][0]["context"]["arguments"] == args
    assert len(state["calls"]) == 1


@pytest.mark.parametrize("missing", ["operation"])
def test_memory_required_fields_are_enforced(memory, missing):
    args = {key: value for key, value in SEARCH.items() if key != missing}
    assert "error" in json.loads(invoke(memory, args))
    assert not memory[2]["calls"] and memory[2]["reports"][0]["ok"] is False


@pytest.mark.parametrize(
    "guard", ["unreserved", "wrong-tool", "uncounted", "no-capability", "legacy", "cancelled", "closed"]
)
def test_memory_cannot_bypass_native_reservation_or_authority(memory, guard):
    tools, measurements, state = memory
    measurements.attempted = 0 if guard == "uncounted" else 1
    if guard != "unreserved":
        tools.start("memory-1", "http_fetch" if guard == "wrong-tool" else "foundry_memory", SEARCH)
    if guard == "no-capability":
        measurements.phase["capabilities"] = ["network"]
    if guard == "legacy":
        measurements.phase.pop("executionMode")
    if guard == "cancelled":
        measurements.failure = "Authority revoked"
    if guard == "closed":
        tools.close()
    assert "reserved" in json.loads(tools.invoke_memory("foundry_memory", foundry._foundry_memory, SEARCH))["error"]
    assert not state["policies"] and not state["calls"] and not state["reports"]
    assert measurements.successful == 0


@pytest.mark.parametrize("failure", ["denied", "http", "tool", "failed", "timeout"])
def test_failed_memory_calls_are_not_replayed_or_credited(memory, failure):
    tools, measurements, state = memory
    if failure == "denied":
        state["allowed"] = False
    elif failure == "http":
        state["status"] = 403
    elif failure == "tool":
        state.update(isError=True, result={"status": "completed", "update_id": "never-trust-error-content"})
    elif failure == "timeout":
        state["timeout"] = True
    else:
        state["result"] = {"status": "failed", "update_id": "failed-1"}
    result = json.loads(invoke(memory, UPDATE))
    assert result.get("error") or result.get("isError") or result.get("status") == "failed"
    assert measurements.attempted == 1 and measurements.successful == 0
    assert len(state["calls"]) == (failure != "denied")
    assert [r["ok"] for r in state["reports"]] == ([] if failure == "denied" else [False])
    tools.invoke_memory("foundry_memory", foundry._foundry_memory, UPDATE)
    assert len(state["calls"]) == (failure != "denied")
    assert len(state["policies"]) == 1


@pytest.mark.parametrize("provider", [None, "", "azure-openai", "azure-foundry", "github-models", "github-copilot"])
@pytest.mark.parametrize("endpoint", [None, "", " ", "https://foundry.example/api/projects/reviewed"])
@pytest.mark.parametrize("refused", [False, True])
def test_memory_uses_router_binding_not_legacy_agent_environment(memory, monkeypatch, provider, endpoint, refused):
    for key, value in (("KARS_PROVIDER", provider), ("FOUNDRY_PROJECT_ENDPOINT", endpoint)):
        if value is None:
            monkeypatch.delenv(key, raising=False)
        else:
            monkeypatch.setenv(key, value)
    _, measurements, state = memory
    if refused:
        state.update(isError=True, result={"error": "Memory binding unavailable"})
    result = json.loads(invoke(memory, SEARCH))
    assert len(state["policies"]) == len(state["calls"]) == 1
    assert state["calls"][0]["params"] == {"name": "foundry.memory", "arguments": SEARCH}
    assert measurements.successful == (not refused)
    assert [r["ok"] for r in state["reports"]] == [not refused]
    if refused:
        assert result["isError"] is True
        assert "Memory binding unavailable" in result["content"][0]["text"]
    else:
        assert result == state["result"]


class Registry:
    """Small registration seam double; real native dispatch is tested separately."""

    def __init__(self):
        self.entries = {}

    def register(self, **kwargs):
        entry = dict(
            is_async=False,
            check_fn=None,
            description=None,
            emoji=None,
            max_result_size_chars=None,
            dynamic_schema_overrides=None,
        )
        entry.update(kwargs)
        self.entries[entry["name"]] = SimpleNamespace(**entry)

    def get_entry(self, name):
        return self.entries.get(name)

    def snapshot_registration(self, name):
        return self.get_entry(name)

    def restore_registration(self, name, current, previous):
        if self.get_entry(name) is current:
            if previous is None:
                self.entries.pop(name, None)
            else:
                self.entries[name] = previous

    def get_definitions(self, names, quiet):
        return [{"type": "function", "function": self.entries[name].schema} for name in names]


@pytest.mark.parametrize("caps", [["memory"], [], ["network"]])
@pytest.mark.parametrize("existing", [False, True])
def test_memory_registration_is_explicit_narrow_and_restored(memory, monkeypatch, caps, existing):
    tools, measurements, _ = memory
    measurements.phase = memory_phase(caps)
    registry = Registry()
    if existing:
        registry.register(
            name="foundry_memory",
            toolset="foundry",
            schema=foundry._FOUNDRY_MEMORY_SCHEMA,
            handler=foundry._foundry_memory,
            dynamic_schema_overrides=lambda: pytest.fail("Broad schema"),
        )
    previous = registry.get_entry("foundry_memory")
    monkeypatch.setitem(sys.modules, "tools", SimpleNamespace(file_tools=object()))
    monkeypatch.setattr(tools.spill, "install", lambda: None)
    monkeypatch.setattr(tools.spill, "close", lambda: None)
    monkeypatch.setattr(foundry, "register", lambda *args: pytest.fail("Full Foundry catalog registration"))
    try:
        definitions = tools.install(registry)
        names = {tool["function"]["name"] for tool in definitions}
        assert ("foundry_memory" in names) is ("memory" in caps)
        assert "kars_access_request" in names
        if "memory" in caps:
            assert names == {"foundry_memory", "kars_access_request"}
            memory_definition = next(
                tool["function"] for tool in definitions if tool["function"]["name"] == "foundry_memory"
            )
            assert memory_definition == MEMORY_SCHEMA
            assert registry.get_entry("foundry_memory").dynamic_schema_overrides is None
            measurements.attempted = 1
            tools.start("memory-1", "foundry_memory", SEARCH)
            assert json.loads(registry.get_entry("foundry_memory").handler(SEARCH)) == {"memories": []}
        else:
            assert registry.get_entry("foundry_memory") is previous
        assert not any(name.startswith("foundry_") and name != "foundry_memory" for name in registry.entries)
    finally:
        tools.close()
    assert registry.get_entry("foundry_memory") is previous
    assert registry.get_entry("kars_access_request") is None


def test_memory_registration_is_restored_on_install_failure(memory, monkeypatch):
    tools, _, _ = memory
    registry = Registry()
    monkeypatch.setitem(sys.modules, "tools", SimpleNamespace(file_tools=object()))
    monkeypatch.setattr(tools.spill, "install", lambda: wire.require(False, "Installation failed"))
    monkeypatch.setattr(tools.spill, "close", lambda: None)
    try:
        with pytest.raises(ValueError, match="Installation failed"):
            tools.install(registry)
    finally:
        tools.close()
    assert registry.get_entry("foundry_memory") is None


@pytest.mark.parametrize(
    "operation,outcome",
    [
        (
            "search",
            {
                "backend": "builtin",
                "retrieval": "lexical",
                "scope": "private_owner_role",
                "memories": [{"text": "Helsinki"}],
            },
        ),
        (
            "update",
            {"backend": "builtin", "status": "completed", "memory_id": "durable-id", "approved_knowledge": False},
        ),
    ],
)
def test_builtin_memory_is_router_selected_without_foundry_or_local_files(memory, monkeypatch, operation, outcome):
    monkeypatch.delenv("KARS_PROVIDER", raising=False)
    monkeypatch.delenv("FOUNDRY_PROJECT_ENDPOINT", raising=False)
    tools, measurements, state = memory
    state["result"] = outcome
    args = {"operation": operation, "text": "Helsinki"}
    assert json.loads(invoke(memory, args)) == outcome
    assert state["calls"][0]["params"] == {"name": "foundry.memory", "arguments": args}
    assert measurements.successful == 1
    assert list(tools.root.iterdir()) == []
    assert all("Helsinki" not in json.dumps(report) for report in state["reports"])
