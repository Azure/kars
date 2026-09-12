# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Closed CLI diagnostic vocabulary; never relay executable output or values."""

import json
from .ssa_diagnostics import CONFLICT_KEYS, valid_conflict_facts

SOURCES = {
    **{step: "core-helm-schemas" for step in ("helm-render", "helm-rollback-review", "helm-history-recheck")},
    **{step: "sre-schema-migration" for step in (
        "schema-qualification", "registrar", "schema-inventory", "controller-quiescence",
        "schema-retention", "canonical-target", "canonical-before", "schema-owner", "stored-versions", "migration-recheck")},
    **{step: "sre-migration-data" for step in (
        "data-inventory", "data-list-shape", "data-item-identity", "data-fields", "data-server-validation",
        "data-returned-identity", "data-round-trip", "data-recheck", "new-authorities")},
    **{step: "schema-stage" for step in (
        "schema-plan", "policy-review", "schema-plan-identity", "helm-schema-match",
        "schema-server-preview", "schema-preview-identity", "schema-write", "schema-publication")},
}
KINDS = {"A2AAgent", "EgressApproval", "InferencePolicy", "KarsApproval", "KarsAuthConfig",
         "KarsEval", "KarsMemory", "KarsProfile", "KarsReceipt", "KarsSkill", "KarsSREAction", "KarsTask",
         "KarsTeam", "McpServer", "ToolPolicy", "TrustGraph", "KarsSandbox", "KarsPairing",
         "KarsBudgetAccount", "KarsCredentialGrant", "KarsSRERegistration", "Deployment"}
FIELDS = {"metadata/uid", "metadata/resourceVersion", "metadata/name", "metadata/namespace",
          "metadata/labels", "metadata/annotations", "metadata/ownerReferences", "metadata/finalizers", "spec", "status",
          "spec/envelope/budget/scope", "spec/defaultEnvelope/budget/scope", "spec/blueprint/credentialBindings",
          "spec/blueprint/githubBinding", "spec/roster/*/blueprint/credentialBindings", "spec/roster/*/blueprint/githubBinding",
          "spec/roster/*/envelope/budget/scope", "spec/managed", "spec/credentialsRef", "spec/credentialBindings",
          "spec/githubBinding", "spec/inferenceBudgetRef", "status/serviceObservation",
          "status/conditions/*/observedGeneration", "status/reportConfigMapRef", "status/reportConfigMapUid",
          "status/reportEvidenceDigest", "unrecognized"}
REASONS = {"Forbidden", "Unauthorized", "Invalid", "NotFound", "AlreadyExists", "Conflict",
           "BadRequest", "InternalError", "ServiceUnavailable"}


def schema_preparation_failure(stderr):
    records = []
    prefix = "SRE-SCHEMA-PREPARATION "
    for line in stderr.splitlines():
        if not line.startswith(prefix) or len(line) > 4096:
            continue
        try:
            value = json.loads(line[len(prefix):])
        except ValueError:
            continue
        if not isinstance(value, dict) or set(value) - ({"step", "source", "kind", "field", "shape", "category", "reason"} | CONFLICT_KEYS):
            continue
        step, category = value.get("step"), value.get("category")
        if (not isinstance(step, str) or step not in SOURCES
                or value.get("source") != f"cli/src/lib/{SOURCES[step]}.ts"
                or not isinstance(category, str)
                or category not in {"local-check", "api-rejection", "transport-timeout", "missing-command",
                                    "command-failure", "invalid-json"}):
            continue
        if any(key in value and (not isinstance(value[key], str) or value[key] not in allowed)
               for key, allowed in (("kind", KINDS), ("field", FIELDS), ("reason", REASONS))):
            continue
        if ("reason" in value) != (category == "api-rejection"):
            continue
        if not valid_conflict_facts(value):
            continue
        if "shape" in value:
            shape = value["shape"]
            if (not isinstance(shape, dict) or set(shape) != {"uid", "resourceVersion", "kind", "apiVersion"}
                    or any(not isinstance(item, str) or item not in {"missing", "empty", "string", "other"}
                           for item in shape.values())):
                continue
        records.append(value)
    if len(records) == 1:
        return records[0]
    return {"category": "ambiguous"} if records else None
