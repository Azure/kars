"""kars-runtime-hermes — in-pod adapter that wires Hermes into kars governance.

Public API: just import the package; Hermes' plugin discovery finds
``plugin/__init__.py::register`` at startup.

This top-level module is intentionally minimal — the plugin entry point
lives at ``kars_runtime_hermes.plugin``. We expose only ``__version__``
and the contract-version constant here so tests / external tooling can
introspect without loading the plugin context.
"""

from __future__ import annotations

__version__ = "0.1.0"

# Pinned: kars runtime contract version this adapter implements.
# Must match ``KARS_RUNTIME_CONTRACT_VERSION`` injected by the controller.
# See ``docs/runtimes/CONTRACT.md``.
KARS_RUNTIME_CONTRACT_VERSION = "v1"
