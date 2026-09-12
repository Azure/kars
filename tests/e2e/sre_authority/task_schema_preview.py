# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Early native Task SSA probe; no schema migration or conflict workaround."""

import json

from .canonical_seed import _snapshot
from .common import SYSTEM, command_error_category, require
from .registration_schema import write_report
from .ssa_diagnostics import ssa_conflict
from .task_schema_conflicts import read_task_schema, require_task_owner, task_manager_facts, task_schema_conflict


def payload_helper(h, mode, value=None):
    args = ["node", str(h.root / "tests/e2e/sre_authority/task_schema_payload.mjs"), mode]
    return json.loads(h.run(args, **({"data": json.dumps(value)} if value is not None else {}), timeout=20))


def require_task_payload_helper(h):
    require((h.root / "cli/dist/lib/schema-write-request.js").is_file(),
            "Early Task SSA requires Node.js 22+ and a CLI build: run npm ci && npm run build in cli before the schema tests")
    require(payload_helper(h, "check") == {"ready": True}, "Compiled production schema helper is unavailable")


def task_preview(h, rendered, case):
    require(case in ("before-negatives", "after-owner-restore", "after-schema-restore"), "Unknown Task SSA case")
    current = read_task_schema(h)
    require_task_owner(current)
    request = payload_helper(h, "build", {"rendered": rendered, "current": current})
    require(request.get("args") == ["apply", "--server-side", "--field-manager=helm", "-f", "-", "-o", "json"]
            and isinstance(request.get("input"), str), "Production helper returned an unexpected Task SSA request")
    report = {"kind": "KarsTask", "case": case, "managerFacts": task_manager_facts(current)}
    try:
        result = h.k(*request["args"], "--dry-run=server", "--request-timeout=20s",
                     data=request["input"], expected=None, timeout=25)
        if result.returncode:
            conflict = ssa_conflict(result.stderr)
            report.update(category="api-rejection" if conflict else command_error_category(result.stderr))
            if conflict:
                report.update(reason="Conflict", **conflict)
            write_report(h.root, f"migration-seed-task-ssa-{case}.json", report)
            raise AssertionError("Task SSA server-preview failed; see fixed conflict/manager evidence")
        require(payload_helper(h, "validate", {"rendered": rendered, "current": current, "returned": result.stdout})
                == {"validated": True}, "Production helper did not validate the exact Task preview")
    finally:
        require(read_task_schema(h) == current, "Task SSA preview persisted a schema or field-ownership change")
    report.update(category="accepted", nonPersistent=True)
    write_report(h.root, f"migration-seed-task-ssa-{case}.json", report)


def exercise_task_restore_preview(h):
    require_task_payload_helper(h)
    rendered = h.run(["helm", "template", "kars", str(h.root / "deploy/helm/kars"),
                      "--namespace", SYSTEM, "--show-only", "templates/crd-karstask.yaml"])
    before = _snapshot(h)
    try:
        task_preview(h, rendered, "before-negatives")
        for fault in ("owner", "schema"):
            with task_schema_conflict(h, fault) as changed:
                if fault == "owner":
                    require(changed["metadata"]["annotations"]["meta.helm.sh/release-name"] == "foreign-fixture",
                            "Early owner-negative mutation did not take effect")
                else:
                    require(changed["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["description"]
                            == "Unreviewed public fixture description", "Early schema-negative mutation did not take effect")
            task_preview(h, rendered, f"after-{fault}-restore")
    finally:
        require(_snapshot(h) == before, "Task negative-restore/SSA probe changed CR data, identity or workload intent")
    h.passed("Native Task SSA remained nonpersistent before and after the shared negative fixture restoration")
