#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# CI gate: every `Microsoft.Authorization/roleAssignments` in deploy/bicep must
# name itself with a GUID that incorporates the assignee **principalId**, so an
# identity rotation (e.g. the AKS kubelet identity on cluster recreate, or a
# UAMI re-created with the same name) produces a NEW assignment name — a clean
# CREATE — instead of an UPDATE that ARM rejects with
# `RoleAssignmentUpdateNotPermitted`.
#
# This is the static regression guard for the v0.1.15 idempotency fix. A role
# assignment whose name GUID is derived only from resource `.id`s (stable across
# rotation) is the exact bug that broke `kars up` on existing/recreated RGs.
#
# A name is considered idempotent if its GUID expression references a token
# matching principalId / objectId / spObjectId (case-insensitive) — i.e. the
# assignee principal, not just scope/resource ids.

import glob
import re
import sys

PRINCIPAL_TOKEN = re.compile(r"principal[_]?id|object[_]?id|spobjectid", re.IGNORECASE)
GUID_INLINE = re.compile(r"^\s*name:\s*(guid\(.+)$")
NAME_LINE = re.compile(r"^\s*name:\s*(.+?)\s*$")
VAR_GUID = re.compile(r"^\s*var\s+(\w+)\s*=\s*(guid\(.+)$", re.MULTILINE)
RA_TYPE = "Microsoft.Authorization/roleAssignments@"


def check_file(path: str) -> list[str]:
    errs: list[str] = []
    src = open(path, encoding="utf-8").read()
    lines = src.splitlines()

    # Map: var name -> its `guid(...)` definition (single-line vars).
    var_guid: dict[str, str] = {}
    for m in VAR_GUID.finditer(src):
        var_guid[m.group(1)] = m.group(2)

    for i, line in enumerate(lines):
        if RA_TYPE not in line:
            continue
        # Find the resource's `name:` within the next few lines.
        name_expr = None
        for j in range(i, min(i + 8, len(lines))):
            nm = NAME_LINE.match(lines[j])
            if nm:
                name_expr = nm.group(1)
                break
        loc = f"{path}:{i + 1}"
        if name_expr is None:
            errs.append(f"{loc}: roleAssignment has no resolvable `name:` (cannot verify idempotency)")
            continue
        # Resolve the name to a guid() expression: inline, or via a var.
        if name_expr.startswith("guid("):
            expr = name_expr
        elif name_expr in var_guid:
            expr = var_guid[name_expr]
        else:
            errs.append(
                f"{loc}: roleAssignment name `{name_expr}` is not a guid()/known var — "
                f"cannot verify it includes the principalId"
            )
            continue
        if not PRINCIPAL_TOKEN.search(expr):
            errs.append(
                f"{loc}: roleAssignment name does NOT include a principalId/objectId — "
                f"not idempotent on identity rotation (would risk RoleAssignmentUpdateNotPermitted): "
                f"{expr[:90]}"
            )
    return errs


def main() -> int:
    files = sorted(glob.glob("deploy/bicep/**/*.bicep", recursive=True))
    if not files:
        print("bicep-rbac-idempotency: no .bicep files found under deploy/bicep", file=sys.stderr)
        return 1
    all_errs: list[str] = []
    ra_count = 0
    for f in files:
        ra_count += open(f, encoding="utf-8").read().count(RA_TYPE)
        all_errs.extend(check_file(f))
    if all_errs:
        print("❌ Bicep RBAC idempotency gate FAILED:\n")
        for e in all_errs:
            print(f"  - {e}")
        print(
            "\nFix: name role assignments `guid(<scope>, <principalId>, <roleDefId>)`. "
            "If the principalId is a runtime reference(), pass it as a string param via a "
            "module (BCP120) — see deploy/bicep/modules/sandbox-rbac.bicep."
        )
        return 1
    print(f"✅ Bicep RBAC idempotency gate OK — {ra_count} role assignment(s) across {len(files)} file(s), all include a principalId.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
