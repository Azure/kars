"""Operator enrollment transport checks, not live native authority evidence."""

import copy
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import Mock, patch

import enrollment
import operator_diagnostics
from native_api import BRIDGE, CORE, WRITER, Failure, core, resource


class EnrollmentTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="bridge-enrollment-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.cli = self.root / ".native/core/cli/dist/index.js"
        self.cli.parent.mkdir(parents=True)
        self.cli.touch()
        definition = self.root / ".native/core/deploy/helm/kars/files/private-consumption.json"
        definition.parent.mkdir(parents=True)
        definition.write_text(json.dumps({"controllers": ["deployment-controller", "replicaset-controller"]}))
        self.namespace = "native-workspace"
        self.keys = ["SLACK_BOT_TOKEN"]
        self.writer = {"metadata": {"name": WRITER, "namespace": BRIDGE, "uid": "writer"}}
        self.review = {
            "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsCredentialGrant",
            "metadata": {"name": "workspace", "namespace": self.namespace},
            "spec": {"workspaceUid": "workspace", "enabled": True,
                     "writers": [{"namespace": BRIDGE, "name": WRITER, "uid": "writer"}],
                     "agentKeys": self.keys,
                     "privateActivation": {"phase": "reviewed"}},
        }
        self.grant = copy.deepcopy(self.review)
        self.grant["metadata"]["uid"] = "grant"
        self.grant["spec"]["privateActivation"]["phase"] = "qualified"
        self.commands, self.reads, self.review_files = [], [], []
        self.admin = types.SimpleNamespace(get=self.get, optional=self.optional)
        self.setup = types.SimpleNamespace(admin=self.admin, ready_grant=lambda _namespace: self.grant)

    def get(self, path):
        self.reads.append(path)
        if path == "/api/v1/namespaces/" + self.namespace:
            return {"metadata": {"name": self.namespace, "uid": "workspace"}}
        if path == core(BRIDGE, "serviceaccounts", WRITER):
            return self.writer
        self.fail("Unexpected administrative read")

    def optional(self, path):
        self.reads.append(path)
        if path == resource(self.namespace, "karscredentialgrants", "workspace"):
            return None
        if path.startswith("/api/v1/namespaces/kube-system/serviceaccounts/"):
            return {"metadata": {"uid": "real-profile-account"}}
        self.fail("Unexpected optional read")

    def command(self, *args, **kwargs):
        self.commands.append((args, kwargs))
        if args[4] == "preview":
            return json.dumps(self.review)
        self.assertEqual(args[4], "apply")
        self.assertEqual(json.loads(Path(args[5]).read_text()), self.review)
        return "Reviewed grant recorded"

    def private_file(self, name, data):
        path = self.root / name
        path.write_text(data)
        path.chmod(0o600)
        self.review_files.append(path)
        return path

    def enroll(self, *, previous=None, keys=None, observations=()):
        with patch.object(enrollment, "ROOT", self.root), \
             patch.object(enrollment, "CLI", self.cli), \
             patch.object(operator_diagnostics, "command", side_effect=self.command), \
             patch.object(enrollment, "private_file", side_effect=self.private_file):
            return enrollment.enroll(self.setup, self.namespace, self.writer,
                                     self.keys if keys is None else keys, previous=previous,
                                     observations=observations)

    def test_uses_real_public_preview_apply_and_then_controller_readiness(self):
        self.assertEqual(self.enroll(), self.grant)
        self.assertEqual(len(self.commands), 2)
        preview, options = self.commands[0]
        self.assertEqual(preview[:5], ("node", str(self.cli), "credentials", "grant", "preview"))
        self.assertIn("--private-root", preview)
        self.assertIn(CORE, preview)
        self.assertIn("service-accounts", preview)
        self.assertIn(f"{BRIDGE}/Deployment/kars-bridge-bff", preview)
        self.assertEqual(preview[-2:], ("--agent-key", "SLACK_BOT_TOKEN"))
        self.assertEqual(options["timeout"], 180)
        self.assertEqual(self.commands[1][1]["timeout"], 360)
        self.assertEqual(self.review["spec"]["privateActivation"]["phase"], "reviewed")
        self.assertEqual(self.review_files[0].stat().st_mode & 0o777, 0o600)

    def test_existing_grant_is_not_adopted_or_updated(self):
        self.admin.optional = lambda _path: self.grant
        with self.assertRaises(Failure):
            self.enroll()
        self.assertEqual(self.commands, [])

    def test_explicit_existing_grant_key_update_uses_reviewed_uid_and_version(self):
        self.grant["metadata"]["resourceVersion"] = "7"
        previous = copy.deepcopy(self.grant)
        self.review["metadata"].update(uid="grant", resourceVersion="7")
        keys = [*self.keys, "DISCORD_BOT_TOKEN"]
        self.review["spec"]["agentKeys"] = keys
        self.admin.optional = lambda _path: self.grant
        original = self.command
        def command(*args, **kwargs):
            result = original(*args, **kwargs)
            if args[4] == "apply":
                self.grant["spec"]["agentKeys"] = keys
                self.grant["metadata"]["resourceVersion"] = "8"
            return result
        self.command = command
        result = self.enroll(previous=previous, keys=keys)
        self.assertEqual(result["metadata"]["uid"], previous["metadata"]["uid"])
        self.assertEqual(result["spec"]["agentKeys"], keys)
        self.assertEqual(previous["spec"]["agentKeys"], ["SLACK_BOT_TOKEN"])
        self.assertEqual(len(self.commands), 2)

    def test_existing_update_rejects_changed_incarnations_and_non_key_authority(self):
        self.grant["metadata"]["resourceVersion"] = "7"
        previous = copy.deepcopy(self.grant)
        self.admin.optional = lambda _path: self.grant
        for field, value in (("uid", "replacement"), ("resourceVersion", "changed")):
            before = copy.deepcopy(self.grant)
            self.grant["metadata"][field] = value
            with self.subTest(field=field), self.assertRaises(Failure):
                self.enroll(previous=previous)
            self.grant = before
        self.grant["spec"]["integrationStores"] = [{"secret": {"name": "private"}}]
        with self.assertRaisesRegex(Failure, "only an agent-key grant"):
            self.enroll(previous=copy.deepcopy(self.grant))
        self.assertEqual(self.commands, [])

    def test_observation_target_and_private_consumer_are_explicitly_reviewed(self):
        target = {"kind": "KarsSandbox", "namespace": self.namespace, "name": "observer", "uid": "target"}
        self.review["spec"]["observationTargets"] = [target]
        self.grant["spec"]["observationTargets"] = [target]
        self.enroll(observations=[target])
        args = self.commands[0][0]
        self.assertIn("--observe", args)
        self.assertIn("observer", args)
        self.assertIn("kars-observer/Deployment/observer", args)
        self.commands.clear()
        self.review["spec"]["observationTargets"] = [{**target, "uid": "replaced"}]
        with self.assertRaises(Failure):
            self.enroll(observations=[target])
        self.assertEqual(len(self.commands), 1)
        self.commands.clear()
        with self.assertRaises(Failure):
            self.enroll(observations=[{**target, "namespace": "other"}])
        self.assertEqual(self.commands, [])

    def test_operator_apply_failure_cannot_become_ready_or_a_direct_create_fallback(self):
        self.setup.ready_grant = Mock(return_value=self.grant)
        original = self.command
        def command(*args, **kwargs):
            if args[4] == "apply":
                raise Failure("Operator activation failed")
            return original(*args, **kwargs)
        self.command = command
        with self.assertRaisesRegex(Failure, "Operator activation failed"):
            self.enroll()
        self.setup.ready_grant.assert_not_called()

    def test_changed_review_identity_or_keys_never_reaches_apply(self):
        for field, value in (("workspaceUid", "other"), ("writers", []),
                             ("agentKeys", []), ("privateActivation", {"phase": "qualified"}),
                             ("privateActivation", None)):
            original = copy.deepcopy(self.review)
            self.review["spec"][field] = value
            self.commands.clear()
            with self.subTest(field=field, value=value), self.assertRaises(Failure):
                self.enroll()
            self.assertEqual(len(self.commands), 1)
            self.review = original

    def test_unqualified_or_changed_recorded_grant_is_rejected(self):
        for field, value in (("workspaceUid", "other"), ("writers", []),
                             ("agentKeys", []), ("privateActivation", {"phase": "reviewed"})):
            original = copy.deepcopy(self.grant)
            self.grant["spec"][field] = value
            with self.subTest(field=field), self.assertRaises(Failure):
                self.enroll()
            self.grant = original

    def test_missing_exact_core_cli_does_not_fall_back_to_fabricated_authority(self):
        self.cli.unlink()
        with self.assertRaises(Failure):
            self.enroll()
        self.assertEqual(self.commands, [])


if __name__ == "__main__":
    unittest.main()
