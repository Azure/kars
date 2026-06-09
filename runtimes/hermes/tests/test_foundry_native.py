"""Unit tests for the 4 new native Foundry tools in Hermes plugin
(web_search, code_execute, image_generation, file_search) plus the
shared `_extract_response_text` helper.

Pins the registration surface (5 tools registered when Foundry is
configured), the schema enum the LLM sees, arg validation, and the
shared text-extraction helper that mirrors OpenClaw's parsing at
`runtimes/openclaw/src/core/agt-tools/foundry.ts:504-514`.
"""

from __future__ import annotations

import json
from typing import Any
from unittest import mock

import pytest

from kars_runtime_hermes.plugin import foundry


class _FakeCtx:
    def __init__(self) -> None:
        self.tools: dict[str, Any] = {}

    def register_tool(self, *, name: str, **kw: Any) -> None:
        self.tools[name] = kw


# ── registration ───────────────────────────────────────────────────


def test_register_skipped_for_slim_provider(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KARS_PROVIDER", "github-copilot")
    ctx = _FakeCtx()
    foundry.register(ctx)
    assert ctx.tools == {}


def test_register_skipped_without_foundry_endpoint(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("KARS_PROVIDER", raising=False)
    monkeypatch.delenv("FOUNDRY_PROJECT_ENDPOINT", raising=False)
    ctx = _FakeCtx()
    foundry.register(ctx)
    assert ctx.tools == {}


def test_register_native_five(monkeypatch: pytest.MonkeyPatch) -> None:
    """When Foundry is bound, all 5 native tools register. This is the
    delta vs the pre-parity state (only foundry_memory)."""
    monkeypatch.setenv("FOUNDRY_PROJECT_ENDPOINT", "https://x.example/api/projects/y")
    monkeypatch.delenv("KARS_PROVIDER", raising=False)
    ctx = _FakeCtx()
    foundry.register(ctx)
    assert set(ctx.tools.keys()) == {
        "foundry_memory",
        "foundry_web_search",
        "foundry_code_execute",
        "foundry_image_generation",
        "foundry_file_search",
    }


# ── _extract_response_text helper ──────────────────────────────────


def test_extract_text_from_message_output() -> None:
    """The canonical /openai/responses shape: output[].type=='message',
    content[].type=='output_text' or 'text'. Multi-message responses
    are joined with double newlines (matches OpenClaw at
    foundry.ts:518)."""
    payload = {
        "output": [
            {
                "type": "message",
                "content": [
                    {"type": "output_text", "text": "Answer part one."},
                    {"type": "text", "text": "Answer part two."},
                ],
            },
            {
                "type": "message",
                "content": [{"type": "output_text", "text": "Citation [1]"}],
            },
        ]
    }
    out = foundry._extract_response_text(payload)
    assert "Answer part one." in out
    assert "Answer part two." in out
    assert "Citation [1]" in out


def test_extract_text_falls_back_to_json_when_no_text() -> None:
    """A non-text content type (e.g. only function_call output) falls
    back to JSON pretty-print so the LLM at least sees the raw shape."""
    payload = {"output": [{"type": "tool_call", "name": "x"}]}
    out = foundry._extract_response_text(payload)
    # Should be JSON, not empty
    assert "tool_call" in out
    json.loads(out)  # round-trips as JSON


# ── arg validation ─────────────────────────────────────────────────


def test_web_search_requires_query() -> None:
    parsed = json.loads(foundry._foundry_web_search({}))
    assert "query" in parsed["error"]


def test_code_execute_requires_input() -> None:
    parsed = json.loads(foundry._foundry_code_execute({}))
    assert "input" in parsed["error"]


def test_code_execute_blocks_sandbox_path_writes() -> None:
    """Same guard OpenClaw applies — code that writes to /sandbox/ or
    /tmp/ inside the Foundry container fails silently (different FS).
    Reject before sending."""
    parsed = json.loads(
        foundry._foundry_code_execute(
            {"input": "open('/sandbox/x.txt', 'w').write('hi')"}
        )
    )
    assert "/mnt/data/" in parsed["error"]


def test_image_generation_requires_prompt() -> None:
    parsed = json.loads(foundry._foundry_image_generation({}))
    assert "prompt" in parsed["error"]


def test_file_search_requires_query_and_vector_store() -> None:
    parsed = json.loads(foundry._foundry_file_search({"vector_store_id": "x"}))
    assert "query" in parsed["error"]
    parsed = json.loads(foundry._foundry_file_search({"query": "x"}))
    assert "vector_store_id" in parsed["error"]


# ── wire shape — body sent to the router ───────────────────────────


def test_web_search_body_shape() -> None:
    """The request body MUST match the OpenClaw shape so the router
    treats both runtimes identically. Specifically: model + input +
    tools[{bing_grounding}] + store=false."""
    captured: dict[str, Any] = {}

    def _capture(method: str, path: str, *, json: Any = None, params: dict | None = None) -> Any:
        captured["method"] = method
        captured["path"] = path
        captured["body"] = json
        return {
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Sample result"}],
                }
            ]
        }

    # Patch BOTH router calls — the connection-discovery GET and the POST
    with mock.patch(
        "kars_runtime_hermes.plugin.foundry.router_client.call_json",
        side_effect=_capture,
    ):
        result = foundry._foundry_web_search({"query": "weather in Seattle"})
    assert "Sample result" in result
    assert captured["method"] == "POST"
    assert captured["path"].startswith("/openai/responses")
    body = captured["body"]
    assert body["model"] == "gpt-4.1"
    assert body["input"] == "weather in Seattle"
    assert body["store"] is False
    assert body["tools"][0]["type"] == "bing_grounding"


def test_code_execute_body_forces_tool_choice() -> None:
    """Without `tool_choice: code_interpreter` the model often
    describes code in prose without running it (foundry.ts:97)."""
    captured: dict[str, Any] = {}

    def _capture(method: str, path: str, *, json: Any = None, params: dict | None = None) -> Any:
        captured["body"] = json
        return {"output": []}

    with mock.patch(
        "kars_runtime_hermes.plugin.foundry.router_client.call_json",
        side_effect=_capture,
    ):
        foundry._foundry_code_execute({"input": "print(2+2)"})
    body = captured["body"]
    assert body["tools"][0]["type"] == "code_interpreter"
    assert body["tool_choice"] == {"type": "code_interpreter"}
