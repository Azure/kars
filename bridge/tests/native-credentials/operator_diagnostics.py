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
    "Private activation staging requires the existing cluster-scoped credential operator authority": "operator-authority",
    "Explicit credential-grant operator permission is required": "operator-authority",
}
MODULES = (
    "commands/credential-grants", "lib/private-activation",
    "lib/private-activation-retirement", "lib/kube-bootstrap", "lib/kube-context",
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
    if stage not in ("preview", "apply"):
        raise Failure("Unknown native operator enrollment stage")
    try:
        return command(*args, timeout=timeout)
    except CommandFailure as error:
        raise Failure(
            f"Native operator {stage} failed: {category(error.stderr)} "
            f"(source={source_location(error.stderr)})"
        ) from None
