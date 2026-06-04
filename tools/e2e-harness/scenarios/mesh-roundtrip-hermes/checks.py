# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Checks for the mesh-roundtrip-hermes scenario.

A single check: the transcript must contain the exact line
``RECEIVED:echo(mesh-pong-hermes): hello-from-ping`` produced when
the Hermes LLM successfully completes the mesh send → await →
base64-decode round-trip.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Callable

if TYPE_CHECKING:
    from verify import Context


EXPECTED = "RECEIVED:echo(mesh-pong-hermes): hello-from-ping"


def _expected_line_present(ctx: "Context") -> tuple[bool, str]:
    transcript = ctx.transcript or ""
    if EXPECTED in transcript:
        return True, f"transcript contains the expected line: {EXPECTED!r}"
    # Surface up to 400 chars of transcript tail for triage.
    tail = transcript[-400:] if transcript else "<empty>"
    return False, (
        f"expected line {EXPECTED!r} not found in transcript "
        f"(last 400 chars: {tail!r})"
    )


def _no_failure_marker(ctx: "Context") -> tuple[bool, str]:
    transcript = ctx.transcript or ""
    for marker in ("FAILED:", "TIMEOUT", "Peer .* not found"):
        if marker in transcript:
            tail = transcript[-300:]
            return False, f"transcript contains failure marker {marker!r}: ...{tail!r}"
    return True, "no FAILED:/TIMEOUT markers in transcript"


def _daemon_saw_message(ctx: "Context") -> tuple[bool, str]:
    """Sanity check: the echo daemon's stdout (captured by the
    Hermes-aware platform driver) should contain ECHO_GOT + ECHO_REPLIED
    lines for the one message the parent sent."""
    daemon_log = ctx.out_dir / "daemon-mesh-pong-hermes.log"
    if not daemon_log.exists():
        return False, f"daemon log not produced at {daemon_log}"
    content = daemon_log.read_text(errors="replace")
    if "ECHO_GOT" not in content:
        return False, f"daemon never logged ECHO_GOT (full: {content[-400:]!r})"
    if "ECHO_REPLIED" not in content:
        return False, f"daemon logged inbound but never replied (full: {content[-400:]!r})"
    return True, "daemon logged ECHO_GOT + ECHO_REPLIED"


def get_checks() -> list[tuple[str, Callable[["Context"], tuple[bool, str]]]]:
    return [
        ("Echo daemon received + replied", _daemon_saw_message),
        ("No failure markers in transcript", _no_failure_marker),
        ("Expected RECEIVED line in transcript", _expected_line_present),
    ]
