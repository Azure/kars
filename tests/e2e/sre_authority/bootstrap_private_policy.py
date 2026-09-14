# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Public-source-only attribution of private-consumption admission failures."""

import ast
from functools import cache
import json
from pathlib import Path
import re

POLICY = "kars-private-consumption"
PREFIX = "kars.azure.com/private-"
CEL_BINDINGS = {"namespaceObject", "object", "oldObject", "request", "variables", "authorizer", "params"}
STRINGS = re.compile(r"'(?:\\.|[^'\\])*'|\"(?:\\.|[^\"\\])*\"")
POLICY_REFERENCE = re.compile(r"""\b(?:ValidatingAdmissionPolicy|policy)\s+['"]([^'"\r\n]{1,253})['"]""", re.IGNORECASE)
LOCATION = re.compile(r"(?<![\w./-])spec\.(matchConditions|variables|validations)\[(\d{1,3})\]\.expression(?![\w.-])")
MISSING_KEY = re.compile(
    r"""\bno such key:\s*(?:'([^'\r\n]{1,256})'|"([^"\r\n]{1,256})"|([^\s,;)\]}'"]{1,256}))""",
    re.IGNORECASE,
)


@cache
def public_contract():
    path = Path(__file__).resolve().parents[3] / "deploy/helm/kars/files/private-consumption.json"
    bundle = json.loads(path.read_text())
    policies = [obj for obj in bundle["objects"]
                if obj["kind"] == "ValidatingAdmissionPolicy" and obj["metadata"]["name"] == POLICY]
    if len(policies) != 1:
        raise RuntimeError("Canonical private-consumption diagnostic contract is unavailable")
    policy, keys, sites = policies[0], set(), {}
    for section in ("matchConditions", "variables", "validations"):
        for index, definition in enumerate(policy["spec"].get(section, [])):
            expression = definition["expression"]
            field = f"spec.{section}[{index}].expression"
            site = {"field": field}
            if "name" in definition:
                site["name"] = definition["name"]
            sites[field] = (site, expression, section)
            # Remove CEL string literals before recognizing field selections.
            code = STRINGS.sub("''", expression)
            keys.update(re.findall(r"\.\??([A-Za-z_][A-Za-z0-9_]*)\b(?!\s*\()", code))
            keys.update(set(re.findall(r"\b[A-Za-z_][A-Za-z0-9_]*\b", code)) & CEL_BINDINGS)
            for match in STRINGS.finditer(expression):
                value = ast.literal_eval(match[0])
                if value.startswith(PREFIX) and not value.endswith("-"):
                    keys.add(value)
                elif re.match(r"\s+in\b", expression[match.end():]):
                    keys.add(value)
    for controller in bundle["controllers"]:
        keys.add(f"{PREFIX}{controller}-uid")
    return policy, frozenset(keys), sites


def private_policy_failure(message, policies, causes=()):
    references = set(POLICY_REFERENCE.findall(message))
    if POLICY not in references:
        return {}
    result = {"policy": POLICY, "missingKeys": [], "missingKeyClassification": "unclassified",
              "expressionSites": [], "expressionClassification": "unclassified"}
    canonical, known_keys, sites = public_contract()
    installed = policies.get(POLICY)
    if (references != {POLICY} or not isinstance(installed, dict)
            or installed.get("spec") != canonical["spec"]):
        return {"publicPolicyFailure": result}

    fragments = [(message[:65536], None)]
    if isinstance(causes, list):
        for cause in causes[:32]:
            if not isinstance(cause, dict):
                continue
            text, field = cause.get("message"), cause.get("field")
            fragments.append((text[:4096] if isinstance(text, str) else "",
                              field if isinstance(field, str) and len(field) <= 128 else None))
    keys, unknown_key, locations = set(), False, set()
    for text, supplied_field in fragments:
        named = set(POLICY_REFERENCE.findall(text))
        if named and named != {POLICY}:
            continue
        for match in MISSING_KEY.finditer(text):
            key = next(value for value in match.groups() if value is not None)
            if key in known_keys:
                keys.add(key)
            else:
                unknown_key = True
        if supplied_field in sites:
            locations.add(supplied_field)
        for match in LOCATION.finditer(text):
            if match[0] in sites:
                locations.add(match[0])
        for field, (site, expression, section) in sites.items():
            for quote in ("'", '"', "`"):
                if f"expression {quote}{expression}{quote}" in text:
                    locations.add(field)
                name = site.get("name")
                label = "match condition" if section == "matchConditions" else "variable"
                if name and (f"{label} {quote}{name}{quote}" in text
                             or f"expression {quote}{name}{quote}" in text):
                    locations.add(field)
                if section == "variables" and f"expression {quote}variables.{name}{quote}" in text:
                    locations.add(field)
    result["missingKeys"] = sorted(keys)[:8]
    if keys and not unknown_key and len(keys) <= 8:
        result["missingKeyClassification"] = "known-public-key"
    result["expressionSites"] = [sites[field][0] for field in sorted(locations)[:8]]
    if locations and len(locations) <= 8:
        result["expressionClassification"] = "provided-public-location"
    return {"publicPolicyFailure": result}
