# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Smoke tests for the MAF-Python AgentMesh first-class tool wrappers."""

from __future__ import annotations

import pytest

from kars_runtime_maf_python import build_mesh_tools, register_mesh_tools


def test_mesh_tools_re_exported_via_package_init():
    assert callable(build_mesh_tools)
    assert callable(register_mesh_tools)


def test_register_mesh_tools_requires_tools_attr():
    class Bogus:
        pass

    with pytest.raises(TypeError):
        register_mesh_tools(Bogus())


def test_build_mesh_tools_returns_two_named_tools():
    pytest.importorskip("agent_framework")
    tools = build_mesh_tools()
    names = [
        getattr(t, "__kars_tool_name__", None) or getattr(t, "name", None)
        for t in tools
    ]
    assert sorted(n for n in names if n) == ["mesh_inbox", "mesh_send"]
