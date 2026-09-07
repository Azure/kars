#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Render task admission against reused values, without new-default coalescing."""

import io
import itertools
import os
from pathlib import Path
import subprocess
import tarfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
TEMPLATES = ROOT / "deploy/helm/kars/templates"
OLD_VALUES = (ROOT / "tests/compat/fixtures/task-floor-old-values.yaml").read_text()
ARCHIVE_IDS = itertools.count()
FLOOR = "admission-task-namespace-floor.yaml"


def render(values, legacy_templates=False):
    # The saved release values are the chart defaults in this isolated archive.
    # Passing -f to the current chart would merge in its new defaults and hide
    # the --reuse-values nil-map regression.
    files = {
        "Chart.yaml": "apiVersion: v2\nname: task-floor-compat\nversion: 0.1.0\n",
        "values.yaml": values,
        f"templates/{FLOOR}": (TEMPLATES / FLOOR).read_text(),
    }
    if legacy_templates:
        for name in ["admission-pod-exec-ban.yaml", "admission-sandbox-posture-lock.yaml"]:
            files[f"templates/{name}"] = (TEMPLATES / name).read_text()
    archive = Path(f".task-floor-compat-{os.getpid()}-{next(ARCHIVE_IDS)}.tgz")
    output = archive.open("xb")
    try:
        with output, tarfile.open(fileobj=output, mode="w:gz") as package:
            for name, content in files.items():
                data = content.encode()
                info = tarfile.TarInfo(f"task-floor-compat/{name}")
                info.size = len(data)
                package.addfile(info, io.BytesIO(data))
        return subprocess.run(
            ["helm", "template", "kars", str(archive), "--namespace", "kars-system"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    finally:
        archive.unlink()


class TaskFloorReuseValues(unittest.TestCase):
    def assert_floor(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("name: kars-task-namespace-floor\n", result.stdout)
        self.assertIn("name: kars-task-namespace-floor-binding\n", result.stdout)
        self.assertIn("failurePolicy: Fail", result.stdout)
        self.assertIn("validationActions: [Deny, Audit]", result.stdout)

    def test_old_values_enable_new_floor_without_resetting_old_flags(self):
        result = render(OLD_VALUES, legacy_templates=True)
        self.assert_floor(result)
        self.assertNotIn("name: kars-sandbox-exec-ban", result.stdout)
        self.assertIn("name: kars-sandbox-posture-lock\n", result.stdout)

    def test_absent_parent_map_enables_floor(self):
        self.assert_floor(render("{}\n"))

    def test_null_parent_map_enables_floor(self):
        self.assert_floor(render("admission: null\n"))

    def test_absent_floor_map_enables_floor(self):
        self.assert_floor(render("admission: {}\n"))

    def test_null_floor_map_enables_floor(self):
        self.assert_floor(render("admission:\n  taskNamespaceFloor: null\n"))

    def test_absent_enabled_flag_enables_floor(self):
        self.assert_floor(render("admission:\n  taskNamespaceFloor: {}\n"))

    def test_null_enabled_flag_enables_floor(self):
        self.assert_floor(render("admission:\n  taskNamespaceFloor:\n    enabled: null\n"))

    def test_explicit_true_enables_floor(self):
        self.assert_floor(render("admission:\n  taskNamespaceFloor:\n    enabled: true\n"))

    def test_existing_explicit_false_is_preserved(self):
        result = render(OLD_VALUES + "  taskNamespaceFloor:\n    enabled: false\n", True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("name: kars-task-namespace-floor", result.stdout)
        self.assertNotIn("name: kars-sandbox-exec-ban", result.stdout)
        self.assertIn("name: kars-sandbox-posture-lock\n", result.stdout)

    def test_non_boolean_flag_fails_instead_of_disabling_security(self):
        for value in ['"false"', "0"]:
            with self.subTest(value=value):
                result = render(f"admission:\n  taskNamespaceFloor:\n    enabled: {value}\n")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("enabled must be a boolean", result.stderr)


if __name__ == "__main__":
    if Path.cwd().resolve() != ROOT:
        raise SystemExit("Run this test from the repository root.")
    unittest.main()
