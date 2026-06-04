# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Checks for the multi-agent exec-brief-hermes scenario."""

from __future__ import annotations

from typing import TYPE_CHECKING, Callable

if TYPE_CHECKING:
    from verify import Context


def _expected_final_line(ctx: "Context") -> tuple[bool, str]:
    transcript = ctx.transcript or ""
    if "EXEC_BRIEF_DONE" in transcript:
        return True, "transcript contains EXEC_BRIEF_DONE marker"
    tail = transcript[-400:] if transcript else "<empty>"
    return False, f"EXEC_BRIEF_DONE marker missing (tail: {tail!r})"


def _no_failure_marker(ctx: "Context") -> tuple[bool, str]:
    transcript = ctx.transcript or ""
    for marker in ("FAILED:", "TIMEOUT:"):
        if marker in transcript:
            tail = transcript[-300:]
            return False, f"transcript contains failure marker {marker!r}: ...{tail!r}"
    return True, "no FAILED:/TIMEOUT markers in transcript"


def _sub_agents_spawned(ctx: "Context") -> tuple[bool, str]:
    """At least the three sub-agent gateway logs should exist (the
    aks.sh collect step writes one per sub-sandbox declared in
    config.sh's SCENARIO_SUB_SANDBOXES)."""
    missing = []
    for sub in ("analyst", "viz", "writer"):
        log_path = ctx.out_dir / f"{sub}-gateway.log"
        if not log_path.exists() or log_path.stat().st_size == 0:
            missing.append(sub)
    if missing:
        return False, f"sub-agent gateway logs missing or empty: {missing}"
    return True, "analyst/viz/writer gateway logs all present"


def _mesh_traffic_observed(ctx: "Context") -> tuple[bool, str]:
    """At least one of the sub-agent logs should mention kars_mesh_send
    or kars_mesh_await — proves the real Python AGT MeshClient fired
    inside the sub-agent pods."""
    for sub in ("analyst", "viz", "writer"):
        log_path = ctx.out_dir / f"{sub}-gateway.log"
        if not log_path.exists():
            continue
        content = log_path.read_text(errors="replace")
        if "kars_mesh_send" in content or "kars_mesh_await" in content:
            return True, f"{sub} gateway log contains mesh-tool reference"
    return False, "no mesh-tool references in any sub-agent gateway log"


def get_checks() -> list[tuple[str, Callable[["Context"], tuple[bool, str]]]]:
    return [
        ("Sub-agents spawned (gateway logs present)", _sub_agents_spawned),
        ("Mesh traffic observed in a sub-agent log", _mesh_traffic_observed),
        ("No FAILED:/TIMEOUT: in transcript", _no_failure_marker),
        ("Transcript ends with EXEC_BRIEF_DONE marker", _expected_final_line),
    ]
