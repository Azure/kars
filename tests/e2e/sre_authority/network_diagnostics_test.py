# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
import json
from pathlib import Path
import re
import unittest

from sre_authority.common import STANDIN
from sre_authority.network_diagnostics import guard_variant


class NetworkDiagnosticTests(unittest.TestCase):
    def fixture(self):
        root = Path(__file__).resolve().parents[3]
        source = (root / "controller/src/reconciler/pod_spec.rs").read_text()
        body = source.split("pub(crate) fn build_egress_guard_command", 1)[1].split("\n    cmd\n", 1)[0]
        expected = "".join(json.loads(value) for value in re.findall(r'cmd\.push_str\(\s*("(?:\\.|[^"\\])*")\s*\)', body))
        return root, {"spec": {"initContainers": [{"name": "egress-guard", "image": STANDIN,
            "command": ["sh", "-c", expected], "securityContext": {"runAsUser": 0}}]}}

    def test_full_control_reproduces_actual_guard_without_mutating_it(self):
        root, pod = self.fixture()
        original = copy.deepcopy(pod)
        self.assertEqual(guard_variant(root, pod, "full"), pod["spec"]["initContainers"][0])
        self.assertEqual(pod, original)

    def test_filter_control_preserves_uid_drop_and_legacy_control_preserves_every_rule(self):
        root, pod = self.fixture()
        filtered = guard_variant(root, pod, "filter-only")["command"][2]
        self.assertIn("--uid-owner 1000 -j DROP", filtered)
        self.assertNotIn("-t nat", filtered)
        legacy = guard_variant(root, pod, "legacy-full")["command"][2]
        expected = pod["spec"]["initContainers"][0]["command"][2]
        self.assertEqual(legacy.split("; ", 1)[1], expected.replace("iptables ", "iptables-legacy "))
        self.assertIn("|| exit 42", legacy)

    def test_unreviewed_command_image_or_extra_mount_is_never_executed(self):
        root, pod = self.fixture()
        for key, value in (("image", "other-image"), ("command", ["sh", "-c", "unreviewed"]),
                           ("volumeMounts", [{"name": "private"}]), ("envFrom", [{"secretRef": {"name": "private"}}])):
            changed = copy.deepcopy(pod)
            changed["spec"]["initContainers"][0][key] = value
            with self.assertRaises(AssertionError):
                guard_variant(root, changed, "full")
