"""9 Foundry tool wrappers — Phase A1.7 — STUB.

Will register: foundry_code_execute, foundry_download_file,
foundry_image_generation, foundry_web_search, foundry_file_search,
foundry_memory, foundry_conversations, foundry_evaluations,
foundry_deployments, foundry_agents.

Registration is gated on KARS_PROVIDER: skipped when value is
``github-models`` or ``github-copilot`` (slim modes — no Foundry project).
"""

from __future__ import annotations

import os
from typing import Any


def register(ctx: Any) -> None:  # noqa: ANN401
    """Stub — full impl ships in the A1.7 commit."""
    provider = os.environ.get("KARS_PROVIDER", "")
    if provider in {"github-models", "github-copilot"}:
        return
    # TODO(A1.7): register each foundry_* tool with a router HTTP wrapper
    pass
