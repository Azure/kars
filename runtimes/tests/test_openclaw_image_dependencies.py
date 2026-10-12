# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
RECIPE = ROOT / "sandbox-images/openclaw/Dockerfile.base"
SUMS = ROOT / "sandbox-images/openclaw/node24-SHASUMS256.txt"
VERIFIER = ROOT / "sandbox-images/openclaw/verify-node-archive.sh"
VERIFY = 'sh /tmp/verify-node-archive.sh "/tmp/$NODE_ARCHIVE" /tmp/node-SHASUMS256.txt'


class OpenClawImageDependencyTests(unittest.TestCase):
    def test_1password_checksum_command_rejects_corrupt_and_missing_archives(self):
        block = next(
            block for block in RECIPE.read_text().split("\n\n")
            if "https://cache.agilebits.com/" in block
        )
        check = next(
            line.strip().removesuffix(" && \\") for line in block.splitlines()
            if "| sha256sum " in line
        )
        payload = b"signed archive fixture"
        for case in ("valid", "corrupt", "missing", "malformed"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                archive = Path(directory) / "op.zip"
                if case != "missing":
                    archive.write_bytes(b"changed" if case == "corrupt" else payload)
                digest = hashlib.sha256(payload).hexdigest()
                result = subprocess.run(
                    ["sh", "-c", check.replace("/tmp/op.zip", str(archive))],
                    env=dict(os.environ, OP_SHA256=digest[:-1] if case == "malformed" else digest),
                    text=True, capture_output=True, timeout=10,
                )
                if case == "valid":
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    self.assertNotEqual(result.returncode, 0, result.stdout)

    def test_1password_security_release_is_checked_before_extraction(self):
        block = next(
            block for block in RECIPE.read_text().split("\n\n")
            if "https://cache.agilebits.com/" in block
        )
        self.assertIn('OP_VER="2.40.0"', block)
        check = 'echo "$OP_SHA256  /tmp/op.zip" | sha256sum --check --strict -'
        self.assertLess(block.index("curl "), block.index(check))
        self.assertLess(block.index(check), block.index("unzip -qo"))
        self.assertIn("rm /tmp/op.zip", block)
        subprocess.run(["sh", "-n", "-c", block.removeprefix(
            "# Install 1Password CLI (enables 1password skill)\nRUN "
        )], check=True, capture_output=True)
        setup = block.split("\nRUN ", 1)[1].split("    curl ", 1)[0]
        setup = setup.replace("\\\n", " ").rstrip().removesuffix("&&").rstrip()
        checksums = {
            "arm64": "0e8ac99ee93d661aa725dc24a5ef8bf344741d224064a5dfb469dc689faec86a",
            "amd64": "74277219e8da60958c00f9aee9d2023225e98fdda8bfd2156a5d9e85e0edaab3",
        }
        for target, arch in (
            ("arm64", "arm64"), ("aarch64", "arm64"),
            ("amd64", "amd64"), ("x86_64", "amd64"), ("s390x", None),
        ):
            with self.subTest(target=target):
                result = subprocess.run(
                    ["sh", "-c", setup + ' && printf "%s %s" "$OP_ARCH" "$OP_SHA256"'],
                    env=dict(os.environ, TARGETARCH=target),
                    capture_output=True, text=True, timeout=10,
                )
                if arch is None:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("Unsupported 1Password architecture", result.stderr)
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout, f"{arch} {checksums[arch]}")

    def test_node_checksums_cover_both_supported_architectures_and_recipe_version(self):
        version = re.search(
            r"^ARG NODE_VERSION=(\d+\.\d+\.\d+)$", RECIPE.read_text(), re.M
        )[1]
        entries = [line.split("  ") for line in SUMS.read_text().splitlines()]
        self.assertEqual(len(entries), 2)
        self.assertEqual(
            {name for _, name in entries},
            {f"node-v{version}-linux-{arch}.tar.gz" for arch in ("arm64", "x64")},
        )
        for digest, _ in entries:
            self.assertRegex(digest, r"^[0-9a-f]{64}$")

    def test_each_node_download_is_checked_before_extraction(self):
        recipe = RECIPE.read_text()
        self.assertEqual(
            recipe.count(
                "COPY sandbox-images/openclaw/node24-SHASUMS256.txt /tmp/node-SHASUMS256.txt"
            ),
            2,
        )
        blocks = [
            block
            for block in recipe.split("\n\n")
            if "curl " in block and "https://nodejs.org/dist/v" in block
        ]
        self.assertEqual(len(blocks), 2)
        self.assertEqual(
            recipe.count(
                "COPY sandbox-images/openclaw/verify-node-archive.sh /tmp/verify-node-archive.sh"
            ),
            2,
        )
        for block in blocks:
            self.assertIn(VERIFY, block)
            self.assertLess(block.index("curl "), block.index(VERIFY))
            self.assertLess(block.index(VERIFY), block.index("tar xzf"))
            self.assertIn('rm "/tmp/$NODE_ARCHIVE" /tmp/node-SHASUMS256.txt', block)

    def test_architecture_selection_rejects_unsupported_targets(self):
        assignments = re.findall(r"^RUN (NODEJS_ARCH=.*?) &&", RECIPE.read_text(), re.M)
        self.assertEqual(len(assignments), 2)
        cases = {
            "arm64": "arm64",
            "aarch64": "arm64",
            "amd64": "x64",
            "x86_64": "x64",
            "s390x": None,
        }
        for assignment in assignments:
            for target, expected in cases.items():
                with self.subTest(target=target, assignment=assignment):
                    result = subprocess.run(
                        ["sh", "-c", assignment + ' && printf "%s" "$NODEJS_ARCH"'],
                        env=dict(os.environ, TARGETARCH=target),
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    if expected is None:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn("Unsupported Node.js architecture", result.stderr)
                    else:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertEqual(result.stdout, expected)

    def test_real_checksum_command_rejects_wrong_missing_unlisted_and_malformed_inputs(
        self,
    ):
        self.assertTrue(
            VERIFY in RECIPE.read_text(),
            "Image recipe must use the tested checksum command",
        )
        payload = b"checksum test fixture"
        digest = hashlib.sha256(payload).hexdigest()
        filenames = {
            arch: f"node-v24.21.0-linux-{arch}.tar.gz" for arch in ("arm64", "x64")
        }
        for case in (
            "arm64",
            "x64",
            "wrong",
            "missing",
            "unlisted",
            "malformed",
            "duplicate",
            "missing-manifest",
        ):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / "archive storage"
                root.mkdir()
                checksum = digest[:-1] if case == "malformed" else digest
                data = "".join(f"{checksum}  {name}\n" for name in filenames.values())
                if case == "duplicate":
                    data += f"{checksum}  {filenames['arm64']}\n"
                manifest = root / "node-SHASUMS256.txt"
                if case != "missing-manifest":
                    manifest.write_text(data)
                name = filenames["x64" if case == "x64" else "arm64"]
                if case == "unlisted":
                    name = "node-v0.0.0-linux-arm64.tar.gz"
                if case != "missing":
                    (root / name).write_bytes(
                        b"corrupt archive" if case == "wrong" else payload
                    )
                result = subprocess.run(
                    ["sh", str(VERIFIER), str(root / name), str(manifest)],
                    cwd=root,
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
                if case in ("arm64", "x64"):
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    self.assertNotEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
