# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
FILES = {
    "apps/dashvalidator/pkg/validator/prometheus/parser.go": "parser.ParseExpr(query)",
    "pkg/tsdb/grafana-pyroscope-datasource/instance.go": "parser.ParseMetricSelector(labelSelector)",
}


class ParserMigrationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for relative, expression in FILES.items():
            path = self.root / relative
            path.parent.mkdir(parents=True)
            path.write_text(expression + "\n")

    def apply(self):
        return subprocess.run([sys.executable, "-B", str(HERE / "patch-parser.py"), str(self.root)],
                              capture_output=True, text=True, check=False)

    def test_default_parser_semantics_and_provenance(self):
        result = self.apply()
        self.assertEqual(result.returncode, 0, result.stderr)
        records = json.loads(result.stdout)["promqlParserCompatibility"]
        self.assertEqual({record["path"] for record in records}, set(FILES))
        for relative, expression in FILES.items():
            self.assertEqual((self.root / relative).read_text(),
                             expression.replace("parser.", "parser.NewParser(parser.Options{}).", 1) + "\n")
        for record in records:
            self.assertEqual(len(record["before"]), 64)
            self.assertEqual(len(record["after"]), 64)
            self.assertNotEqual(record["before"], record["after"])

    def test_replay_is_rejected_without_writes(self):
        self.assertEqual(self.apply().returncode, 0)
        before = {name: (self.root / name).read_bytes() for name in FILES}
        result = self.apply()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unexpected upstream parser source", result.stderr)
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in FILES})

    def test_later_invalid_file_does_not_partially_patch_first(self):
        relative = next(reversed(FILES))
        (self.root / relative).write_text(FILES[relative] + "\n" + FILES[relative])
        before = {name: (self.root / name).read_bytes() for name in FILES}
        self.assertNotEqual(self.apply().returncode, 0)
        self.assertEqual(before, {name: (self.root / name).read_bytes() for name in FILES})


class CacheProtocolTests(unittest.TestCase):
    def run_protocol(self, payload):
        return subprocess.run([sys.executable, "-B", str(HERE / "no-cache.py")],
                              input=payload, capture_output=True, text=True, check=False)

    def test_only_close_is_advertised_and_acknowledged(self):
        result = self.run_protocol('\n{"ID": 17, "Command": "close"}\n')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([json.loads(line) for line in result.stdout.splitlines()],
                         [{"ID": 0, "KnownCommands": ["close"]}, {"ID": 17}])

    def test_unexpected_command_fails_explicitly(self):
        result = self.run_protocol('{"ID": 3, "Command": "put"}\n')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Unexpected command", result.stderr)

    def test_missing_close_fails_explicitly(self):
        result = self.run_protocol("")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("without closing", result.stderr)


if __name__ == "__main__":
    unittest.main()
