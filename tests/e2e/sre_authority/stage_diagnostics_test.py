# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import json
from pathlib import Path
import re
import subprocess
import tempfile
import time
import types
import unittest
from unittest.mock import patch

from sre_authority.common import Harness
from sre_authority.stage_diagnostics import STAGES, collect_stage_workloads, last_stage, pod_facts


class StageDiagnosticsTests(unittest.TestCase):
    def test_progress_is_last_fixed_phase_not_raw_output(self):
        self.assertEqual(last_stage("PRIVATE\nSRE-STAGE-PROGRESS registrar\nSRE-STAGE-PROGRESS helm-upgrade\nPRIVATE"), "helm-upgrade")
        for text in ("PRIVATE", "SRE-STAGE-PROGRESS PRIVATE", "SRE-STAGE-PROGRESS helm-upgrade PRIVATE",
                     "SRE-STAGE-PROGRESS PRIVATE\nSRE-STAGE-PROGRESS helm-upgrade"):
            self.assertEqual(last_stage(text), "unclassified")

    def test_vocabulary_matches_production_type(self):
        source = (Path(__file__).resolve().parents[3] / "cli/src/lib/sre-stage.ts").read_text()
        declaration = re.search(r"type StagePhase = (.*?);", source, re.S).group(1)
        self.assertEqual(set(re.findall(r'"([a-z-]+)"', declaration)), STAGES)

    def test_timeout_preserves_progress_for_both_command_and_phase_limits(self):
        for remaining, timeout, limit in ((20, 10, "command"), (10, 20, "phase")):
            for force_kill in (False, True):
                with self.subTest(limit=limit, force_kill=force_kill), tempfile.TemporaryDirectory() as directory:
                    h = Harness.__new__(Harness)
                    h.root = h.work = Path(directory)
                    h.deadline = time.monotonic() + remaining
                    stderr = "PRIVATE\nSRE-STAGE-PROGRESS helm-upgrade\nPRIVATE"
                    sequence = [subprocess.TimeoutExpired("PRIVATE", timeout)]
                    if force_kill:
                        sequence.append(subprocess.TimeoutExpired("PRIVATE", 5))
                    sequence.append(("PRIVATE", stderr))
                    with patch("sre_authority.common.subprocess.Popen") as popen, patch("sre_authority.common.os.killpg"), patch("builtins.print") as printed:
                        popen.return_value.communicate.side_effect = sequence
                        with self.assertRaisesRegex(AssertionError, "Command exceeded its bounded timeout"):
                            h.run(["PRIVATE"], timeout=timeout)
                        facts = json.loads(printed.call_args.args[1])
                        self.assertEqual(facts, {"limit": limit, "stage": "helm-upgrade"})
                        self.assertNotIn("PRIVATE", str(printed.call_args_list))

    def test_real_killed_child_retains_progress_without_private_output(self):
        with tempfile.TemporaryDirectory() as directory:
            h = Harness.__new__(Harness)
            h.root = h.work = Path(directory)
            h.deadline = time.monotonic() + 20
            code = "import sys,time; print('PRIVATE', flush=True); print('SRE-STAGE-PROGRESS helm-upgrade', file=sys.stderr, flush=True); time.sleep(10)"
            with patch("builtins.print") as printed, self.assertRaisesRegex(AssertionError, "Command exceeded its bounded timeout"):
                h.run(["python3", "-c", code], timeout=1)
            self.assertEqual(json.loads(printed.call_args.args[1]), {"limit": "command", "stage": "helm-upgrade"})
            self.assertNotIn("PRIVATE", str(printed.call_args_list))

    def test_pod_status_filters_messages_specs_and_unknown_reasons(self):
        pod = {"metadata": {"name": "headlamp-abc", "annotations": {"private": "PRIVATE"}},
               "spec": {"env": "PRIVATE"}, "status": {"phase": "Pending", "message": "PRIVATE",
               "conditions": [{"type": "PodScheduled", "status": "False", "reason": "Unschedulable", "message": "PRIVATE"}],
               "containerStatuses": [{"name": "PRIVATE", "ready": False, "restartCount": 2,
                   "state": {"waiting": {"reason": "ImagePullBackOff", "message": "PRIVATE"}}},
                   {"state": {"terminated": {"reason": "PRIVATE", "exitCode": 139}}}]}}
        result = pod_facts(pod)
        self.assertNotIn("PRIVATE", json.dumps(result))
        self.assertEqual(result["containers"][0]["reason"], "ImagePullBackOff")
        self.assertEqual(result["containers"][1]["exitCode"], 139)
        self.assertEqual(result["conditions"][0]["reason"], "Unschedulable")

    def test_collects_only_fixed_namespaces_and_handles_api_failure_without_leak(self):
        calls = []
        def query(*args, **kwargs):
            calls.append((args, kwargs))
            if args[3] == "headlamp":
                raise RuntimeError("PRIVATE")
            return json.dumps({"items": []})
        with patch("builtins.print") as printed:
            collect_stage_workloads(types.SimpleNamespace(k=query))
        self.assertEqual([call[0][3] for call in calls], ["kars-system", "headlamp"])
        self.assertTrue(all(call[1] == {"timeout": 10} for call in calls))
        self.assertNotIn("PRIVATE", str(printed.call_args_list))
        self.assertTrue(json.loads(printed.call_args.args[1])["unavailable"])
