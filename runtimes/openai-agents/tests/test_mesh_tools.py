# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Smoke tests for the AgentMesh first-class tool wrappers.

These tests import :mod:`mesh_tools` and verify the module is wired
through the package ``__init__``. Building the actual ``function_tool``
objects requires the ``agents`` SDK at runtime; the build is exercised
in the integration test harness and skipped here when the SDK is
unavailable.
"""

from __future__ import annotations

import pytest

from azureclaw_runtime_openai_agents import build_mesh_tools, register_mesh_tools


def test_mesh_tools_re_exported_via_package_init():
    # Re-exports must be present so user code can do
    # `from azureclaw_runtime_openai_agents import build_mesh_tools`.
    assert callable(build_mesh_tools)
    assert callable(register_mesh_tools)


def test_register_mesh_tools_requires_tools_attr():
    class Bogus:
        pass

    with pytest.raises(TypeError):
        register_mesh_tools(Bogus())


def test_build_mesh_tools_returns_two_named_tools():
    pytest.importorskip("agents")
    tools = build_mesh_tools()
    names = [getattr(t, "__azureclaw_tool_name__", None) for t in tools]
    assert sorted(names) == ["mesh_inbox", "mesh_send"]


def test_register_mesh_tools_idempotent():
    pytest.importorskip("agents")

    class FakeAgent:
        def __init__(self):
            self.tools = []

    agent = FakeAgent()
    register_mesh_tools(agent)
    after_first = len(agent.tools)
    register_mesh_tools(agent)
    after_second = len(agent.tools)
    assert after_first == 2
    assert after_second == 2, "second registration must be a no-op"
