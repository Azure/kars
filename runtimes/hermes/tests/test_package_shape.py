"""Basic import-shape sanity tests — runs in CI without Hermes installed."""

from __future__ import annotations


def test_top_level_package_importable() -> None:
    """The top-level package must import without a Hermes context.

    Hermes' plugin loader imports the plugin module at startup; CI tests
    must mirror that import path without booting Hermes itself.
    """
    import kars_runtime_hermes

    assert kars_runtime_hermes.__version__
    assert kars_runtime_hermes.KARS_RUNTIME_CONTRACT_VERSION == "v1"


def test_plugin_entry_callable() -> None:
    """The plugin's register() function exists and is callable.

    We don't actually call it (would need a Hermes ctx mock) — just shape-
    check the contract.
    """
    from kars_runtime_hermes.plugin import register

    assert callable(register)


def test_mesh_stubs_have_act2_message() -> None:
    """Mesh stubs must contain the canonical Act 2 message so the LLM and
    operators know mesh isn't available yet."""
    from kars_runtime_hermes.plugin.mesh_stubs import _ACT2_ERROR

    msg = _ACT2_ERROR["error"]
    assert "Mesh" in msg
    assert "v0.5.2" in msg
    assert "Act 2" in msg
    assert "Foundry Memory Store" in msg or "Foundry Conversations" in msg


def test_plugin_manifest_lists_required_tools() -> None:
    """plugin.yaml MUST advertise the full kars tool set; missing tools
    cause Hermes to silently reject the registration."""
    import importlib.resources

    raw = (importlib.resources.files("kars_runtime_hermes.plugin") / "plugin.yaml").read_text()

    required_tools = [
        "kars_spawn",
        "kars_spawn_status",
        "kars_spawn_destroy",
        "kars_spawn_list",
        "kars_discover",
        "kars_handoff_request",
        "kars_handoff_confirm",
        "kars_handoff_status",
        "kars_mesh_send",
        "kars_mesh_inbox",
        "kars_mesh_await",
        "kars_mesh_transfer_file",
        "http_fetch",
        "foundry_memory",
        "foundry_agents",
    ]
    for tool in required_tools:
        assert tool in raw, f"Tool '{tool}' missing from plugin.yaml"


def test_plugin_manifest_lists_governance_hook() -> None:
    """The AGT pre_tool_call hook is the single most important contract
    item — if missing, every tool runs ungoverned."""
    import importlib.resources

    raw = (importlib.resources.files("kars_runtime_hermes.plugin") / "plugin.yaml").read_text()
    assert "pre_tool_call" in raw
