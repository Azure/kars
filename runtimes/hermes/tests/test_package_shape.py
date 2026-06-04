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


def test_mesh_module_is_real_not_stub() -> None:
    """Act 2.1 — the mesh tools are backed by the real Python AGT
    MeshClient (via kars-agt-mesh). The old Act-1 stub module is gone
    and the new mesh.py exports `register` + the four tool handlers."""
    from kars_runtime_hermes.plugin import mesh

    # Real module — not the stub
    assert hasattr(mesh, "_get_or_init_client")
    assert hasattr(mesh, "_kars_mesh_send")
    assert hasattr(mesh, "register")
    # _MESH_TOOLS table lists all four mesh tools we register
    assert len(mesh._MESH_TOOLS) == 4
    tool_names = [t[0] for t in mesh._MESH_TOOLS]
    assert "kars_mesh_send" in tool_names
    assert "kars_mesh_inbox" in tool_names
    assert "kars_mesh_await" in tool_names
    assert "kars_mesh_transfer_file" in tool_names


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
