"""Use the exact core operator's schema lifecycle before installing its policies."""

import json

from native_api import CORE, ROOT, Failure, require
from operator_diagnostics import operator_command

CLI = ROOT / ".native/core/cli/dist/index.js"
CHART = ".native/core/deploy/helm/kars"


def prepare_schemas(values, context):
    require(CLI.is_file(), "The exact core CLI must be built before schema preparation")
    raw = operator_command(
        "schemas", "node", str(CLI), "schemas", "prepare",
        "--release", "kars", "--namespace", CORE, "--chart", CHART,
        "--context", context, "--ownership", "helm", "--timeout", "120",
        "--values", str(values), timeout=180,
    )
    try:
        result = json.loads(raw)
    except json.JSONDecodeError:
        raise Failure("Core schema preparation did not return a JSON result") from None
    require(
        isinstance(result, dict) and result.get("published") is True
        and type(result.get("schemas")) is int and result["schemas"] > 0
        and result.get("release") == "kars" and result.get("namespace") == CORE
        and result.get("ownership") == "helm",
        "Core schema preparation did not acknowledge the exact owned schema operation",
    )
    return {"schemas": result["schemas"], "published": True}
