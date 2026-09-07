#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail
cd "$(dirname "$0")/../../../.."
python3 - <<'PY'
import io
import json
from pathlib import Path
import re
import subprocess
import tarfile

chart = "deploy/helm/kars"
template = "templates/admission-envelope-write-lock.yaml"
command = ["helm", "template", "kars", chart, "--namespace", "tenant-control"]
rendered = subprocess.check_output(command + ["--show-only", template], text=True)
for gate in ["tierRaised", "ceilingRaised", "depthRaised", "tokenBudgetRaised",
             "usdBudgetRaised", "toolPolicyChanged", "egressRefChanged", "statusChanged"]:
    assert f"- name: {gate}" in rendered, gate
    assert f'expression: "!variables.{gate}"' in rendered, gate
for axis in ["tokens", "usdMicros"]:
    old = f"variables.oldEnv.?budget.?{axis}.orValue(0)"
    new = f"variables.newEnv.?budget.?{axis}.orValue(0)"
    assert f"{old} > 0 &&" in rendered
    assert f"({new} <= 0 ||" in rendered
    assert f"{new} > {old})" in rendered
for reference in ["toolPolicyRef", "egressAllowlistRef"]:
    assert f"has(oldObject.spec.envelope.{reference}) &&" in rendered
    assert f"variables.newEnv.?{reference}.?name.orValue('') != variables.oldEnv.?{reference}.?name.orValue('')" in rendered
assert "system:serviceaccount:tenant-control:kars-controller" in rendered
assert 'resources:   ["karstasks", "karsteams", "karstasks/status", "karsteams/status"]' in rendered
assert "failurePolicy: Fail" in rendered
assert "validationActions: [Deny, Audit]" in rendered
disabled = subprocess.check_output(command + ["--set", "admission.envelopeWriteLock.enabled=false"], text=True)
assert "name: kars-envelope-write-lock\n" not in disabled

# Isolate the owned template without chart defaults: --reuse-values can supply
# only the old release's settings, not newly introduced default sections.
# The test archive stays in the repository and is removed after rendering.
archive = Path(chart) / "tests/envelope-write-lock-render.tgz"
created = False
try:
    with archive.open("xb") as output:
        created = True
        with tarfile.open(fileobj=output, mode="w:gz") as package:
            contents = {
                "Chart.yaml": b"apiVersion: v2\nname: lock-regression\nversion: 0.1.0\n",
                template: (Path(chart) / template).read_bytes(),
                "templates/retained-values.yaml": (
                    'apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: retained-values\n'
                    'data:\n  retained.json: {{ toJson .Values | quote }}\n'
                ).encode(),
            }
            for name, content in contents.items():
                info = tarfile.TarInfo("lock-regression/" + name)
                info.size = len(content)
                package.addfile(info, io.BytesIO(content))
    old_values = json.loads((Path(chart) / "tests/fixtures/envelope-write-lock-old-values.json").read_text())
    cases = [
        ({}, True),
        ({"admission": None}, True),
        (old_values, True),
        ({"admission": {"envelopeWriteLock": {}}}, True),
        ({"admission": {"envelopeWriteLock": {"enabled": None}}}, True),
        ({"admission": {"envelopeWriteLock": {"enabled": False}}}, False),
        ({"admission": {"envelopeWriteLock": {"enabled": True}}}, True),
    ]
    for values, enabled in cases:
        actual = subprocess.check_output(
            ["helm", "template", "lock-regression", str(archive), "--namespace", "tenant-control", "--values", "-"],
            input=json.dumps(values), text=True,
        )
        assert ("name: kars-envelope-write-lock\n" in actual) == enabled, values
        retained = re.search(r"^\s+retained\.json: (.+)$", actual, re.MULTILINE)
        assert retained, actual
        assert json.loads(json.loads(retained.group(1))) == values, "existing flags were mutated"
finally:
    if created:
        archive.unlink()
print("Envelope write-lock: eight authority gates and seven old-values/security-default/opt-out cases passed")
PY
