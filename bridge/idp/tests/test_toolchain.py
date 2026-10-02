# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from pathlib import Path
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[1]
VERSION = next(line.removeprefix("GO_VERSION=") for line in
               (ROOT / "locks/inputs.lock").read_text().splitlines()
               if line.startswith("GO_VERSION="))
SCRIPT = ROOT / "scripts/verify-toolchain.sh"


class ToolchainContracts(unittest.TestCase):
    def verify(self, resolver, builder):
        return subprocess.run(["sh", str(SCRIPT), VERSION, resolver, builder],
                              capture_output=True, text=True)

    def test_each_native_target_can_use_either_resolver_architecture(self):
        for resolver in ("amd64", "arm64"):
            for builder in ("amd64", "arm64"):
                with self.subTest(resolver=resolver, builder=builder):
                    result = self.verify(f"go version {VERSION} linux/{resolver}",
                                         f"go version {VERSION} linux/{builder}")
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_unpinned_or_unsupported_resolver_and_builder_fail_closed(self):
        valid = f"go version {VERSION} linux/amd64"
        for invalid in ("", "go version go1.25.0 linux/amd64",
                        f"go version {VERSION} darwin/arm64",
                        f"go version {VERSION} linux/386",
                        f"go version {VERSION} linux/arm64 extra",
                        f"go version {VERSION} linux/amd64\n{valid}"):
            for resolver, builder in ((invalid, valid), (valid, invalid)):
                with self.subTest(resolver=resolver, builder=builder):
                    result = self.verify(resolver, builder)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Unsupported or unpinned Go toolchain", result.stderr)

    def test_shipping_verifier_checks_actual_compiler_and_retains_lock_checks(self):
        verifier = (ROOT / "scripts/verify-locks.sh").read_text()
        self.assertIn('sh /packaging/scripts/verify-toolchain.sh "$GO_VERSION" '
                      '"$(cat /locks/toolchain.txt)" "$(go version)"', verifier)
        self.assertIn("sha256sum --check --strict SHA256SUMS", verifier)
        self.assertIn("cmp /packaging/locks/requests.txt /locks/requests.txt", verifier)
        self.assertIn("scripts/verify-toolchain.sh /packaging/scripts/",
                      (ROOT / "Dockerfile").read_text())
        self.assertIn("go version -m /out/dex > /out/doc/build-info.txt",
                      (ROOT / "scripts/build.sh").read_text())


if __name__ == "__main__":
    unittest.main()
