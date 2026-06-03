"""AGT policy gate — Phase A1.4 — STUB.

When complete, this module will register a ``pre_tool_call`` hook that
POSTs ``/agt/evaluate`` with the action verb for the about-to-run tool
and short-circuits the call if the policy engine denies.

Full implementation: subsequent commit.
"""

from __future__ import annotations

from typing import Any


def register(ctx: Any) -> None:  # noqa: ANN401
    """Stub — full impl ships in the A1.4 commit."""
    # TODO(A1.4): ctx.register_hook("pre_tool_call", evaluate_or_deny)
    pass
