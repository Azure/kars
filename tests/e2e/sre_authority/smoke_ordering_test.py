# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Smoke assertions wait for their own asynchronous controller resources."""

from pathlib import Path
import re
import subprocess
import unittest


class SmokeOrderingTests(unittest.TestCase):
    def test_resource_checks_use_bounded_exact_waits_and_still_fail_when_absent(self):
        source = (Path(__file__).resolve().parents[1] / "run.sh").read_text()
        for function, kind, name in [
            ("test_networkpolicy_created", "networkpolicy", "sandbox-policy"),
            ("test_serviceaccount_created", "serviceaccount", "sandbox"),
        ]:
            match = re.search(rf"^{function}\(\) \{{\n.*?^\}}", source, re.MULTILINE | re.DOTALL)
            self.assertIsNotNone(match)
            for available in [True, False]:
                with self.subTest(function=function, available=available):
                    script = f"""
wait_for_resource() {{
    printf 'WAIT %s %s %s %s\\n' "$1" "$2" "$3" "$4"
    return {0 if available else 1}
}}
pass() {{ printf 'PASS %s\\n' "$1"; }}
fail() {{ printf 'FAIL %s\\n' "$1"; return 1; }}
{match.group(0)}
{function}
"""
                    result = subprocess.run(["bash", "-c", script], text=True, capture_output=True,
                                            timeout=5, check=False)
                    self.assertEqual(result.returncode, 0 if available else 1)
                    self.assertIn(f"WAIT {kind} {name} kars-e2e-test 30\n", result.stdout)
                    self.assertIn("PASS " if available else "FAIL ", result.stdout)


if __name__ == "__main__":
    unittest.main()
