# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


WRAPPER = Path(__file__).resolve().parents[2] / "sandbox-images/mcp-playwright/browser-platform.sh"


class PlaywrightPlatformTests(unittest.TestCase):
    def invoke(self, architecture):
        with tempfile.TemporaryDirectory() as directory:
            uname = Path(directory) / "uname"
            uname.write_text(f"#!/bin/sh\nprintf '%s\\n' '{architecture}'\n")
            uname.chmod(0o700)
            return subprocess.run(
                ["/bin/sh", str(WRAPPER), "/bin/sh", "-c",
                 'printf "%s\\n" "$PLAYWRIGHT_HOST_PLATFORM_OVERRIDE" "$@"',
                 "platform-probe", "argument with spaces"],
                env={**os.environ, "PATH": directory,
                     "PLAYWRIGHT_HOST_PLATFORM_OVERRIDE": "inherited-invalid-platform"},
                capture_output=True, text=True, timeout=5, check=False,
            )

    def test_x64_uses_playwright_architecture_qualified_platform(self):
        result = self.invoke("x86_64")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "ubuntu24.04-x64\nargument with spaces\n")

    def test_arm64_keeps_qualified_platform(self):
        result = self.invoke("aarch64")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "ubuntu24.04-arm64\nargument with spaces\n")

    def test_unknown_architecture_never_launches_command(self):
        result = self.invoke("riscv64")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("Unsupported managed browser architecture", result.stderr)


if __name__ == "__main__":
    unittest.main()
