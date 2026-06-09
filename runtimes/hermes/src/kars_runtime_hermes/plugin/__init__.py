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

    SRE-mode containment (per docs/blueprints/07-kars-sre-proposal.md §7.8):
    when ``SRE_ENABLED=true`` is set on the sandbox pod (the env is
    written exclusively by deploy/helm/kars/templates/sre.yaml on the
    ``sre`` KarsSandbox), this entry point:

      - SKIPS registering the kars_spawn family   (§7.8.5)
      - SKIPS registering the kars_mesh_* family  (§7.8.6 — also enforced
        at the NetworkPolicy layer; the deregistration is layer 2)
      - REGISTERS the sre_* tool surface          (sre.py)

    Standard Hermes sandboxes never have ``KARS_SRE_ENABLED`` set and
    therefore get the full standard tool surface (spawn, mesh) with no
    SRE tools.
    """
    from . import sre  # noqa: PLC0415 — lazy import

    sre_mode = sre.is_enabled()
    if sre_mode:
        logger.info(
            "SRE_ENABLED=true detected — entering SRE-mode plugin "
            "registration (no kars_spawn, no kars_mesh_*, sre_* tools "
            "active)"
        )

    # Phase A1.4 — register the pre_tool_call governance hook first
    from . import governance  # noqa: PLC0415 — lazy import

    governance.register(ctx)

    # Phase A1.5 — sub-agent spawn family (HTTP-only against router).
    # SKIPPED in SRE mode per §7.8.5 — the SRE agent must not spawn
    # sub-agents (sub-agents would inherit the kars-sre namespace's
    # RBAC, breaking privilege containment).
    if not sre_mode:
        from . import spawn  # noqa: PLC0415

        spawn.register(ctx)
    else:
        logger.info("§7.8.5 — skipping kars_spawn family registration (SRE mode)")

    # Phase A1.6 — kars_discover (registry HTTP proxy). SKIPPED in SRE
    # mode — the SRE agent doesn't need to find peers (it has no peers).
    if not sre_mode:
        from . import discover  # noqa: PLC0415

        discover.register(ctx)

    # Phase A1.7 — 9 Foundry tool wrappers (HTTP-only; gated when KARS_PROVIDER
    # is a slim/github mode). Retained in SRE mode — the SRE agent may
    # still use Foundry memory + content-safety + inference.
    from . import foundry  # noqa: PLC0415

    foundry.register(ctx)

    # Always-on: http_fetch via /egress/fetch.
    # Retained in SRE mode — the egress NetworkPolicy in sre.yaml is the
    # actual outbound gate; http_fetch's value to the SRE agent is
    # zero today but it's harmless and may be useful for future
    # source-grounding (Slice 5).
    from . import http_fetch  # noqa: PLC0415

    http_fetch.register(ctx)

    # Phase A2.1 — real AGT MeshClient (replaces mesh_stubs).
    # SKIPPED in SRE mode per §7.8.6 — the SRE agent is not on the mesh
    # at all (no DID, no relay socket, not in the registry). The
    # NetworkPolicy in sre.yaml blocks the agentmesh namespace too, so
    # this is one of three enforcement layers (spec env / plugin code /
    # network policy).
    if not sre_mode:
        from . import mesh  # noqa: PLC0415

        mesh.register(ctx)
    else:
        logger.info("§7.8.6 — skipping kars_mesh_* family registration (SRE mode)")

    # Phase A2.1 — deregister Hermes' built-in sub-agent / direct-API
    # tools so the LLM sees ONLY kars's governed mesh path. This is the
    # Hermes equivalent of denying OpenClaw's `session_send` /
    # `session_spawn` — every sub-agent invocation must go through
    # `kars_spawn` (lands in an isolated K8s pod with AGT trust + content
    # safety + token budget + audit chain) and every inter-agent message
    # must go through `kars_mesh_send` (E2E encrypted via Signal Protocol
    # with peer trust gating).
    #
    # Hard-deny set, validated against the Hermes 0.15.2 source via
    # docs/internal/security-audits/2026-06-04-hermes-act2-deny-list.md:
    _HERMES_DENY = [
        # tools/delegate_tool.py — spawns child AIAgents with their own
        # creds (potentially a different provider via `delegation.api_key`
        # config), also exposes outbound ACP subprocess transport
        # (`acp_command="copilot"` etc.). Bypasses kars router entirely.
        "delegate_task",
        # tools/mixture_of_agents_tool.py — direct calls to
        # api.openrouter.ai via tools/openrouter_client.py. 5 parallel
        # external API calls per invocation, completely outside our
        # router's auth / token budget / content safety.
        "mixture_of_agents",
        # tools/cronjob_tools.py — schedules unattended agent sessions
        # via the embedded cron dispatcher (gateway log:
        # "Cron ticker started (interval=60s)"). Each triggered job is
        # a full agent run with LLM calls, runs without an active
        # session for the router to intercept.
        "cronjob",
        # tools/kanban_tools.py — the kanban dispatcher is embedded in
        # `hermes gateway run` (`kanban dispatcher: embedded in gateway
        # interval=60.0s`). kanban_create defers worker spawning; the
        # workers it produces make LLM calls outside any active turn.
        "kanban_create",
        # tools/kanban_tools.py — soft inter-agent comm channel:
        # comments from worker A get injected into worker B's system
        # prompt via kb.build_worker_context(). With the dispatcher
        # running, this is A2A in disguise.
        "kanban_comment",
        # tools/send_message_tool.py — outbound HTTP to Telegram /
        # Discord / Slack / WhatsApp APIs from inside the agent
        # container, completely outside the egress-guard's forward
        # proxy. kars users wanting to push to humans should configure
        # the gateway channel (which routes through the same path) or
        # use foundry_conversations.
        "send_message",
    ]
    deregister = getattr(ctx, "deregister_tool", None)
    if callable(deregister):
        for tool_name in _HERMES_DENY:
            try:
                deregister(tool_name)
                logger.debug("Deregistered Hermes built-in: %s", tool_name)
            except Exception as exc:  # noqa: BLE001
                # Tool may not be registered yet (other plugin order),
                # or may already be filtered by the policy layer. Log
                # and continue — the AGT pre_tool_call hook is the
                # second-line block that catches anything we miss here.
                logger.debug(
                    "Could not deregister %s (will rely on AGT hook): %s",
                    tool_name,
                    exc,
                )

    # Handoff orchestration (HTTP-only)
    from . import handoff  # noqa: PLC0415

    handoff.register(ctx)

    # Phase A2.1 — eagerly init the MeshClient at plugin load so the
    # sub-agent is **discoverable** before its first tool call.
    # SKIPPED in SRE mode per §7.8.6 — the SRE agent is not on the mesh
    # at all; eager-init would fail (registry refuses to register a DID
    # whose pod has no relay egress) and the thread would log a noisy
    # error.
    if not sre_mode:
        try:
            from . import mesh as _mesh_module  # noqa: PLC0415

            import threading as _threading  # noqa: PLC0415

            def _eager_mesh_init() -> None:
                try:
                    _mesh_module._get_or_init_client()  # noqa: SLF001
                    logger.info("MeshClient pre-connected at plugin load")
                    # Now start the auto-responder worker (no-op unless
                    # KARS_MESH_AUTO_RESPONDER=1, which the controller sets
                    # on sub-agent containers — parent is not enabled to
                    # avoid the parent looping on its own outbound).
                    try:
                        from . import mesh_worker as _worker  # noqa: PLC0415

                        _worker.start_worker(_mesh_module._get_or_init_client)  # noqa: SLF001
                    except Exception as exc:  # noqa: BLE001
                        logger.warning("Could not start mesh worker: %s", exc)
                except Exception as exc:  # noqa: BLE001
                    logger.warning(
                        "Eager MeshClient init failed (will retry on first tool call): %s",
                        exc,
                    )

            _threading.Thread(
                target=_eager_mesh_init,
                name="kars-mesh-eager-init",
                daemon=True,
            ).start()
        except Exception as exc:  # noqa: BLE001
            logger.warning("Could not schedule eager MeshClient init: %s", exc)

    # SRE-mode-only: register the sre_* tool surface AFTER everything
    # else has registered (so deregister calls in sre.register can find
    # the targets, though Slice 1 doesn't actually deregister anything).
    if sre_mode:
        sre.register(ctx)

    # Trust + signing-counter background pushes
    from . import telemetry  # noqa: PLC0415

    telemetry.register(ctx)

    logger.info(
        "kars-hermes plugin registered (contract v1, sre_mode: %s, mesh: %s, "
        "Hermes built-ins denied: %d)",
        sre_mode,
        "disabled (SRE mode)" if sre_mode else "real (Act 2.1 — kars-agt-mesh)",
        len(_HERMES_DENY),
    )


# Re-export the manifest so ``hermes plugins doctor`` finds it.
__all__ = ["register"]
