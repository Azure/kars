"""Project only fixed categories and allowlisted source locations from CLI errors."""

import json
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
    "lib/private-activation-writer-settle",
    "commands/schemas", "lib/core-helm-schemas", "lib/schema-stage",
    "lib/schema-documents", "lib/schema-discovery",
    "lib/repo-assets",
)
CHECK_PREFIX = "KARS_PRIVATE_LATE_SANDBOX_CHECKS "
CHECK_FIELDS = (
    ("resourceVersionMatch", "rv"),
    ("observedGenerationMatch", "generation"),
    ("phaseRunningMatch", "running"),
    ("readyConditionMatch", "ready"),
)
WRITER_CHECK_PREFIX = "KARS_PRIVATE_WRITER_RECHECK "
WRITER_CHECK_FIELDS = (
    ("projectionMetadataPresent", "metadata-present"),
    ("projectionMetadataMatches", "metadata-matches"),
    ("deploymentTransitionMatches", "deployment"),
)
COMMAND_PREFIX = "KARS_PRIVATE_COMMAND_FAILURE "
COMMAND_PHASES = {"Unscoped", "Review", "Pausing", "Retired", "Rotating", "Restoring", "Qualified"}
COMMAND_OPERATIONS = {"get", "patch", "create", "auth", "other"}
COMMAND_KINDS = {
    "Namespace", "Deployment", "KarsSandbox", "KarsTask", "Secret", "ServiceAccount",
    "Pod", "ReplicaSet", "AdmissionPolicy", "AdmissionBinding", "AuthorizationInventory",
    "AuthorizationCheck", "Other",
}
COMMAND_REASONS = {
    "Unknown", "BadRequest", "Unauthorized", "Forbidden", "NotFound", "AlreadyExists",
    "Conflict", "Invalid", "Timeout", "ServerTimeout", "TooManyRequests", "ServiceUnavailable",
    "InternalError", "MethodNotAllowed", "Gone", "RequestEntityTooLarge", "UnsupportedMediaType",
}


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


def _json_object(payload):
    try:
        # Preserve duplicate keys so ambiguous facts cannot silently overwrite each other.
        pairs = json.loads(payload, object_pairs_hook=lambda values: values)
    except json.JSONDecodeError:
        return None
    if (not isinstance(pairs, list) or not all(isinstance(pair, tuple) and len(pair) == 2
                                             and isinstance(pair[0], str) for pair in pairs)):
        return None
    values = dict(pairs)
    return values if len(values) == len(pairs) else None


def _checks(stderr, prefix, fields):
    lines = [line for line in stderr.splitlines() if line.startswith(prefix)]
    if not lines:
        return ""
    if len(lines) != 1 or len(lines[0]) > 512:
        return "unavailable"
    values = _json_object(lines[0][len(prefix):])
    if (values is None or set(values) != {key for key, _ in fields}
            or not all(isinstance(value, bool) for value in values.values())):
        return "unavailable"
    return ",".join(f"{label}={str(values[key]).lower()}" for key, label in fields)


def sandbox_checks(stderr):
    return _checks(stderr, CHECK_PREFIX, CHECK_FIELDS)


def writer_checks(stderr):
    return _checks(stderr, WRITER_CHECK_PREFIX, WRITER_CHECK_FIELDS)


def command_facts(stderr):
    prefixes = (COMMAND_PREFIX, "PrivateCommandFailure: " + COMMAND_PREFIX)
    payloads = [line[len(prefix):] for line in stderr.splitlines()
                for prefix in prefixes if line.startswith(prefix)]
    if not payloads:
        return ""
    if len(payloads) != 1 or len(payloads[0]) > 512:
        return "unavailable"
    values = _json_object(payloads[0])
    if values is None or set(values) != {"version", "phase", "operation", "resourceKind", "serverReason", "exitCode"}:
        return "unavailable"
    if (isinstance(values["version"], bool) or not isinstance(values["version"], int) or values["version"] != 1
            or not all(isinstance(values[key], str) and values[key] in allowed
                       for key, allowed in (("phase", COMMAND_PHASES), ("operation", COMMAND_OPERATIONS),
                                            ("resourceKind", COMMAND_KINDS), ("serverReason", COMMAND_REASONS)))):
        return "unavailable"
    code = values["exitCode"]
    if code is not None and (isinstance(code, bool) or not isinstance(code, int) or not 0 <= code <= 255):
        return "unavailable"
    exit_code = "unknown" if code is None else str(code)
    return (f"phase={values['phase']},operation={values['operation']},kind={values['resourceKind']},"
            f"reason={values['serverReason']},exit={exit_code}")


def operator_command(stage, *args, timeout):
    if stage not in ("preview", "apply", "schemas"):
        raise Failure("Unknown native operator enrollment stage")
    try:
        return command(*args, timeout=timeout)
    except CommandFailure as error:
        checks = sandbox_checks(error.stderr)
        details = f" (sandbox-checks={checks})" if checks else ""
        recheck = writer_checks(error.stderr)
        if recheck:
            details += f" (writer-recheck={recheck})"
        facts = command_facts(error.stderr)
        if facts:
            details += f" (command={facts})"
        raise Failure(
            f"Native operator {stage} failed: {category(error.stderr)} "
            f"(source={source_location(error.stderr)}){details}"
        ) from None
