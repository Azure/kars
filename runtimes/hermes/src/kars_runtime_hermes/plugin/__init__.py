"""kars — Hermes plugin entry point.

Hermes discovers this plugin by scanning ``$HERMES_HOME/plugins/<name>/``
for an ``__init__.py`` that exports a top-level ``register(ctx)`` function.
The plugin tree is staged into the sandbox image at
``/opt/kars-hermes-stage/plugins/kars/`` and mirrored to ``$HERMES_HOME``
by the entrypoint on every boot.

This module deliberately keeps imports lazy — Hermes' plugin loader
imports it at startup, so any heavy work must defer until ``register()``
is called.
"""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger("kars.hermes")


def register(ctx: Any) -> None:  # noqa: ANN401 — Hermes' ctx is dynamic
    """Hermes plugin entry point.

    Called once at Hermes startup. ``ctx`` is the Hermes plugin context;
    see https://hermes-agent.nousresearch.com/docs/guides/build-a-hermes-plugin
    for the full API surface.

    Act 1 scope: wire the AGT governance gate, kars_spawn family, Foundry
    tool wrappers, http_fetch via egress proxy, and stubs for kars_mesh_*.
    """
    # Phase A1.4 — register the pre_tool_call governance hook first
    from . import governance  # noqa: PLC0415 — lazy import

    governance.register(ctx)

    # Phase A1.5 — sub-agent spawn family (HTTP-only against router)
    from . import spawn  # noqa: PLC0415

    spawn.register(ctx)

    # Phase A1.6 — kars_discover (registry HTTP proxy)
    from . import discover  # noqa: PLC0415

    discover.register(ctx)

    # Phase A1.7 — 9 Foundry tool wrappers (HTTP-only; gated when KARS_PROVIDER
    # is a slim/github mode)
    from . import foundry  # noqa: PLC0415

    foundry.register(ctx)

    # Always-on: http_fetch via /egress/fetch
    from . import http_fetch  # noqa: PLC0415

    http_fetch.register(ctx)

    # Mesh stubs (Act 1) — return informative errors. Act 2 swaps these
    # for real implementations once the Python AGT MeshClient lands.
    from . import mesh_stubs  # noqa: PLC0415

    mesh_stubs.register(ctx)

    # Handoff orchestration (HTTP-only; works in Act 1)
    from . import handoff  # noqa: PLC0415

    handoff.register(ctx)

    # Trust + signing-counter background pushes
    from . import telemetry  # noqa: PLC0415

    telemetry.register(ctx)

    logger.info(
        "kars-hermes plugin registered (contract v1, mesh: %s)",
        "stubs (Act 1)",
    )


# Re-export the manifest so ``hermes plugins doctor`` finds it.
__all__ = ["register"]
