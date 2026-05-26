# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import httpx
import pytest
import respx

from kars_runtime_maf_python import tools


def test_canonical_tool_names_match_router_catalog():
    # Mirrors `inference-router/src/mcp/platform.rs::foundry_tool_catalog`.
    assert tools.FOUNDRY_TOOL_NAMES == (
        "foundry.web_search",
        "foundry.code_execute",
        "foundry.file_search",
        "foundry.memory",
        "foundry.image_generation",
        "foundry.conversations",
        "foundry.evaluations",
        "foundry.deployments",
        "foundry.agents",
    )


def test_build_foundry_tools_returns_nine():
    built = tools.build_foundry_tools()
    assert len(built) == 9
    # MAF FunctionTool exposes `.name`. Names are derived from the
    # canonical tool name with dots replaced by underscores.
    names = [t.name for t in built]
    assert set(names) == {n.replace(".", "_") for n in tools.FOUNDRY_TOOL_NAMES}


@pytest.mark.asyncio
@respx.mock
async def test_mcp_client_call_tool_returns_text():
    respx.post("http://127.0.0.1:8443/platform/mcp").mock(
        return_value=httpx.Response(
            200,
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": "hello world"}],
                    "isError": False,
                },
            },
        )
    )
    client = tools.FoundryMCPClient()
    out = await client.call_tool("foundry.web_search", {"q": "x"})
    await client.aclose()
    assert out == "hello world"


@pytest.mark.asyncio
@respx.mock
async def test_mcp_client_raises_on_jsonrpc_error():
    respx.post("http://127.0.0.1:8443/platform/mcp").mock(
        return_value=httpx.Response(
            200,
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32601, "message": "method not found"},
            },
        )
    )
    client = tools.FoundryMCPClient()
    with pytest.raises(RuntimeError, match="method not found"):
        await client.call_tool("foundry.web_search", {})
    await client.aclose()


@pytest.mark.asyncio
@respx.mock
async def test_mcp_client_raises_on_is_error_result():
    respx.post("http://127.0.0.1:8443/platform/mcp").mock(
        return_value=httpx.Response(
            200,
            json={
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": "deferred wiring"}],
                    "isError": True,
                },
            },
        )
    )
    client = tools.FoundryMCPClient()
    with pytest.raises(RuntimeError, match="deferred wiring"):
        await client.call_tool("foundry.agents", {})
    await client.aclose()


@pytest.mark.asyncio
@respx.mock
async def test_mcp_client_raises_on_http_error():
    respx.post("http://127.0.0.1:8443/platform/mcp").mock(
        return_value=httpx.Response(500)
    )
    client = tools.FoundryMCPClient()
    with pytest.raises(httpx.HTTPStatusError):
        await client.call_tool("foundry.agents", {})
    await client.aclose()


def test_register_foundry_tools_adds_nine_to_agent():
    class _FakeAgent:
        tools = []

    agent = _FakeAgent()
    tools.register_foundry_tools(agent)
    assert len(agent.tools) == 9
    names = {t.name for t in agent.tools}
    assert names == {n.replace(".", "_") for n in tools.FOUNDRY_TOOL_NAMES}


def test_register_foundry_tools_is_idempotent():
    class _FakeAgent:
        tools = []

    agent = _FakeAgent()
    tools.register_foundry_tools(agent)
    tools.register_foundry_tools(agent)
    assert len(agent.tools) == 9


def test_register_foundry_tools_rejects_bad_agent():
    class _NoTools:
        pass

    with pytest.raises(TypeError):
        tools.register_foundry_tools(_NoTools())


def test_register_foundry_tools_handles_none_tools():
    class _Agent:
        tools = None

    agent = _Agent()
    tools.register_foundry_tools(agent)
    assert len(agent.tools) == 9


def test_platform_mcp_url_default():
    assert tools._platform_mcp_url() == tools.DEFAULT_PLATFORM_MCP_URL


def test_platform_mcp_url_env_override(monkeypatch):
    monkeypatch.setenv("KARS_PLATFORM_MCP_URL", "http://elsewhere/mcp")
    assert tools._platform_mcp_url() == "http://elsewhere/mcp"
