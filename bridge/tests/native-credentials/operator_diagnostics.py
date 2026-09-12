"""Project only fixed categories and allowlisted source locations from CLI errors."""

import re

from native_api import CommandFailure, Failure, command

ERRORS = {
    "Private admission differs from the complete required bundle; upgrade core prerequisites before enrollment": "admission-bundle-mismatch",
    "Private admission is not currently observed and type-checked": "admission-not-observed",
    "Private admission changed since review": "admission-review-changed",
    "Private activation metadata is malformed": "malformed-metadata",
    "Private activation inventory is malformed": "malformed-inventory",
    "Private activation requires live API UID/resourceVersion identities": "missing-live-identity",
    "Reviewed private namespace changed": "namespace-review-changed",
    "Consumer execution differs from the reviewed controller template; preserve it for explicit Pod review": "consumer-execution-drift",
    "Private activation staging requires the existing cluster-scoped credential operator authority": "operator-authority",
    "Explicit credential-grant operator permission is required": "operator-authority",
    "Late private runtime retirement changed or is unsupported; preserve the runtime and re-preview its original review": "late-runtime-review",
    "Task authorization must retain its exact production sha256: digest": "late-task-authorization",
    "Customized admin credential mount requires explicit recovery": "late-admin-mount",
    "Late enrollment only retires the reviewed runtime's controller-owned admin token, not host access, privileged tokens or other private authority": "late-authority-unsupported",
    "Unreviewed late private consumer preserved": "late-pod-lineage",
    "Existing observer, TLS or App private material requires its owner-specific rotation; late admin-only enrollment preserved it": "late-existing-private-material",
    "Late private runtime requires its existing controller-owned admin credential; missing material was not adopted": "late-admin-missing",
    "Late private credential provenance is missing or conflicting": "late-admin-provenance",
    "Reviewed runtime has not consumed its current controller-owned admin credential version": "late-admin-version",
    "Customized or missing late private credential keys require explicit operator recovery": "late-admin-keys",
}
MODULES = (
    "commands/credential-grants", "lib/private-activation",
    "lib/private-activation-retirement", "lib/kube-bootstrap", "lib/kube-context",
    "lib/private-activation-continuity",
    "lib/private-activation-guard-retirement",
    "lib/private-activation-late-scope",
    "commands/schemas", "lib/core-helm-schemas", "lib/schema-stage",
    "lib/schema-documents", "lib/schema-discovery",
    "lib/repo-assets",
)


def category(stderr):
    categories = {value for message, value in ERRORS.items()
                  if f"Error: {message}" in stderr.splitlines()}
    return sorted(categories)[0] if categories else "unclassified-cli-error"


def source_location(stderr):
    for line in stderr.splitlines():
        if not re.match(r"\s+at ", line):
            continue
        for module in MODULES:
            match = re.search(r"/cli/dist/" + re.escape(module) + r"\.js:([1-9][0-9]{0,5}):[0-9]+\)?$", line)
            if match:
                return f"{module}:{match[1]}"
    return "unavailable"


def operator_command(stage, *args, timeout):
    if stage not in ("preview", "apply", "schemas"):
        raise Failure("Unknown native operator enrollment stage")
    try:
        return command(*args, timeout=timeout)
    except CommandFailure as error:
        raise Failure(
            f"Native operator {stage} failed: {category(error.stderr)} "
            f"(source={source_location(error.stderr)})"
        ) from None
