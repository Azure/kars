"""Schema-preparation orchestration tests, not live discovery evidence."""

import io
import json
from contextlib import redirect_stdout
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

import boot
import schema_preparation
from native_api import CORE, Failure


class SchemaPreparationTests(unittest.TestCase):
    def test_invokes_the_exact_public_operator_and_validates_its_result(self):
        result = {"schemas": 12, "published": True, "release": "kars",
                  "namespace": CORE, "ownership": "helm"}
        with tempfile.TemporaryDirectory(prefix="native-schema-cli-") as directory:
            cli = Path(directory) / "index.js"
            cli.touch()
            with patch.object(schema_preparation, "CLI", cli), \
                 patch.object(schema_preparation, "operator_command",
                              return_value=json.dumps(result)) as command:
                self.assertEqual(schema_preparation.prepare_schemas("values.json", "kind-native"),
                                 {"schemas": 12, "published": True})
            self.assertEqual(command.call_args.args, (
                "schemas", "node", str(cli), "schemas", "prepare",
                "--release", "kars", "--namespace", CORE,
                "--chart", ".native/core/deploy/helm/kars",
                "--context", "kind-native", "--ownership", "helm",
                "--timeout", "120", "--values", "values.json",
            ))
            self.assertEqual(command.call_args.kwargs, {"timeout": 180})

    def test_failed_or_mismatched_preparation_never_becomes_success(self):
        baseline = {"schemas": 12, "published": True, "release": "kars",
                    "namespace": CORE, "ownership": "helm"}
        with tempfile.TemporaryDirectory(prefix="native-schema-result-") as directory:
            cli = Path(directory) / "index.js"
            cli.touch()
            with patch.object(schema_preparation, "CLI", cli):
                for change in ({"schemas": 0}, {"schemas": True}, {"published": False},
                               {"release": "other"}, {"namespace": "other"},
                               {"ownership": "template"}):
                    with self.subTest(change=change), \
                         patch.object(schema_preparation, "operator_command",
                                      return_value=json.dumps({**baseline, **change})), \
                         self.assertRaises(Failure):
                        schema_preparation.prepare_schemas("values.json", "kind-native")
                with patch.object(schema_preparation, "operator_command",
                                  side_effect=Failure("Operator preparation failed")), \
                     self.assertRaisesRegex(Failure, "Operator preparation failed"):
                    schema_preparation.prepare_schemas("values.json", "kind-native")

    def test_runtime_install_prepares_schemas_before_the_matching_helm_operation(self):
        calls = []
        setup = types.SimpleNamespace(
            namespace=lambda name: {"metadata": {"name": name, "uid": "namespace"}},
            admin=types.SimpleNamespace(patch=lambda *_: None, get=lambda *_: {"items": []}),
        )
        def prepared(values, context):
            calls.append(("prepare", values, context))
            return {"schemas": 12, "published": True}
        def command(*args, **_kwargs):
            calls.append(args)
            return ""
        with patch.object(boot, "prepare_schemas", side_effect=prepared), \
             patch.object(boot, "command", side_effect=command), \
             patch.object(boot, "private_file"), \
             patch.object(boot, "loaded_image", side_effect=lambda name: name + ":latest"), \
             redirect_stdout(io.StringIO()):
            boot.install_core(setup)
        self.assertEqual(calls[0], ("prepare", ".native/core-values.json", "kind-bridge-native"))
        self.assertEqual(calls[1][:4], ("helm", "install", "kars", ".native/core/deploy/helm/kars"))
        self.assertIn(".native/core-values.json", calls[1])
        self.assertEqual(calls[1][-2:], ("--kube-context", "kind-bridge-native"))

    def test_preparation_failure_prevents_helm_install(self):
        setup = types.SimpleNamespace(
            namespace=lambda name: {"metadata": {"name": name, "uid": "namespace"}},
            admin=types.SimpleNamespace(patch=lambda *_: None),
        )
        with patch.object(boot, "prepare_schemas", side_effect=Failure("Preparation refused")), \
             patch.object(boot, "command") as command, patch.object(boot, "private_file"), \
             patch.object(boot, "loaded_image", side_effect=lambda name: name + ":latest"), \
             self.assertRaisesRegex(Failure, "Preparation refused"):
            boot.install_core(setup)
        command.assert_not_called()


if __name__ == "__main__":
    unittest.main()
