# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from pathlib import Path
import subprocess
import tempfile
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
        for invalid in ("", "go version go1.26.8 linux/amd64",
                        "go version go1.27.0 linux/arm64",
                        f"go version {VERSION} darwin/arm64",
                        f"go version {VERSION} linux/386",
                        f"go version {VERSION} linux/arm64 extra",
                        f"go version {VERSION} linux/amd64\n{valid}"):
            for resolver, builder in ((invalid, valid), (valid, invalid)):
                with self.subTest(resolver=resolver, builder=builder):
                    result = self.verify(resolver, builder)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Unsupported or unpinned Go toolchain", result.stderr)

    def test_official_builder_index_matches_the_compiler_pin(self):
        self.assertEqual(VERSION, "go1.27.3")
        builder = "golang:1.27.3-bookworm@sha256:d9b6a1beb9474adc2a1ce842573639b44b2a0dd49b6669f51d3821baf9ec37c9"
        for name in ("Dockerfile", "Dockerfile.locks"):
            recipe = (ROOT / name).read_text()
            self.assertIn(f"FROM {builder} AS source\n", recipe)
            self.assertIn("GOTOOLCHAIN=local", recipe)
            self.assertIn("CGO_ENABLED=1", recipe)
        self.assertIn(builder, (ROOT / "README.md").read_text())

    def test_source_go_requirements_fail_before_resolution_or_rewriting(self):
        script = ROOT / "scripts/verify-source-toolchain.sh"
        with tempfile.TemporaryDirectory(prefix=".toolchain-contract-", dir=ROOT) as directory:
            module = Path(directory) / "go.mod"
            api = Path(directory) / "api.mod"
            api.write_text("module api\n\ngo 1.25.0\n")
            for directive, success in (("1.25.0", True), ("1.27.0", True),
                                       ("1.27.2", True), ("1.27.3", True), ("1.27.4", False), ("1.28.0", False),
                                       ("2.0.0", False), ("invalid", False)):
                module.write_text(f"module root\n\ngo {directive}\n")
                before = module.read_bytes()
                result = subprocess.run(["sh", str(script), VERSION, str(module), str(api)],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, success, result.stderr)
                self.assertEqual(module.read_bytes(), before)
            module.write_text("module root\n\ngo 1.25.0\n")
            api.write_text("module api\n\ngo 1.27.4\n")
            result = subprocess.run(["sh", str(script), VERSION, str(module), str(api)],
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("api.mod requires Go 1.27.4", result.stderr)
        generator = (ROOT / "scripts/generate-locks.sh").read_text()
        self.assertLess(generator.index("verify-source-toolchain.sh"), generator.index("go mod edit"))
        self.assertNotIn('go mod edit -go=1.26.0 -toolchain="$GO_VERSION"\nset --', generator)
        for recipe in ("Dockerfile", "Dockerfile.locks"):
            self.assertIn("scripts/verify-source-toolchain.sh", (ROOT / recipe).read_text())

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
