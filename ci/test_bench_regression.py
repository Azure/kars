# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

import bench_regression


class BenchRegressionTests(unittest.TestCase):
    def compare(self, actual, base=None, groups=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "baselines.json"
            head = root / "head.txt"
            config.write_text(json.dumps({
                "_threshold_pct": 25,
                "groups": {"proxy/check": {"median_ns": 100}} if groups is None else groups,
            }))
            head.write_text(actual)
            args = ["bench_regression.py", str(config), str(head)]
            if base is not None:
                baseline = root / "base.txt"
                baseline.write_text(base)
                args.append(str(baseline))
            output = io.StringIO()
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
                result = bench_regression.main(args)
            return result, output.getvalue()

    def test_exact_threshold_passes(self):
        self.assertEqual(self.compare("test proxy/check ... bench: 125 ns/iter")[0], 0)

    def test_above_threshold_fails(self):
        self.assertEqual(self.compare("test proxy/check ... bench: 126 ns/iter")[0], 1)

    def test_same_runner_baseline_replaces_historical_timing(self):
        self.assertEqual(self.compare(
            "test proxy/check ... bench: 250 ns/iter",
            "test proxy/check ... bench: 200 ns/iter",
        )[0], 0)

    def test_same_runner_regression_still_fails(self):
        self.assertEqual(self.compare(
            "test proxy/check ... bench: 251 ns/iter",
            "test proxy/check ... bench: 200 ns/iter",
        )[0], 1)

    def test_missing_head_measurement_fails(self):
        result, output = self.compare("")
        self.assertEqual(result, 1)
        self.assertIn("missing head measurement", output)

    def test_missing_base_measurement_fails(self):
        result, output = self.compare("test proxy/check ... bench: 100 ns/iter", "")
        self.assertEqual(result, 1)
        self.assertIn("missing base measurement", output)

    def test_empty_configuration_fails(self):
        self.assertEqual(self.compare("", groups={})[0], 1)

    def test_parse_comma_separated_timings(self):
        self.assertEqual(
            bench_regression.parse_bencher("test proxy/check ... bench: 1,234 ns/iter (+/- 5)"),
            {"proxy/check": 1234},
        )


if __name__ == "__main__":
    unittest.main()
