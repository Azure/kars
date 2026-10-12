# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from email.parser import BytesParser
import hashlib
import json
from pathlib import Path
import unittest
from urllib.parse import urlparse
from zipfile import ZipFile


ROOT = Path(__file__).resolve().parents[2]
WHEELS = ROOT / "vendor/sandbox-wheels"


class OpenClawWheelTests(unittest.TestCase):
    def test_checksum_manifest_covers_exact_wheel_inventory(self):
        lines = (WHEELS / "SHA256SUMS").read_text().splitlines()
        entries = [line.split("  ", 1) for line in lines]
        names = [name for _, name in entries]
        self.assertEqual(len(names), len(set(names)))
        self.assertEqual(set(names), {path.name for path in WHEELS.glob("*.whl")})
        for digest, name in entries:
            with self.subTest(wheel=name):
                self.assertRegex(digest, r"^[0-9a-f]{64}$")
                self.assertEqual(Path(name).name, name)
                self.assertEqual(
                    hashlib.sha256((WHEELS / name).read_bytes()).hexdigest(), digest
                )

    def test_security_updates_match_official_hashes_and_actual_metadata(self):
        updates = json.loads((WHEELS / "security-updates.json").read_text())
        self.assertTrue(updates)
        self.assertEqual(len(updates), len({item["filename"] for item in updates}))
        for item in updates:
            with self.subTest(wheel=item["filename"]):
                self.assertEqual(Path(item["filename"]).name, item["filename"])
                source = urlparse(item["url"])
                self.assertEqual(source.scheme, "https")
                self.assertEqual(source.hostname, "files.pythonhosted.org")
                self.assertEqual(Path(source.path).name, item["filename"])
                path = WHEELS / item["filename"]
                self.assertEqual(path.stat().st_size, item["size"])
                self.assertEqual(
                    hashlib.sha256(path.read_bytes()).hexdigest(), item["sha256"]
                )
                with ZipFile(path) as archive:
                    names = [
                        name
                        for name in archive.namelist()
                        if name.endswith(".dist-info/METADATA")
                    ]
                    self.assertEqual(len(names), 1)
                    metadata = BytesParser().parsebytes(archive.read(names[0]))
                self.assertEqual(metadata["Name"].lower(), item["name"])
                self.assertEqual(metadata["Version"], item["version"])
        for name in {item["name"] for item in updates}:
            with self.subTest(distribution=name):
                expected = {
                    item["filename"] for item in updates if item["name"] == name
                }
                self.assertEqual(
                    {path.name for path in WHEELS.glob(name + "-*.whl")}, expected
                )

    def test_image_checks_integrity_and_installed_dependencies(self):
        recipe = (ROOT / "sandbox-images/openclaw/Dockerfile.base").read_text()
        block = recipe.split("COPY vendor/sandbox-wheels/ /tmp/sandbox-wheels/\n", 1)[
            1
        ].split("\n\n", 1)[0]
        self.assertIn("sha256sum --check SHA256SUMS", block)
        self.assertIn("--no-index --find-links=/tmp/sandbox-wheels/", block)
        self.assertLess(block.index("sha256sum --check"), block.index("pip3 install"))
        self.assertLess(block.index("pip3 install"), block.index("pip3 check"))
        self.assertLess(
            block.index("pip3 check"), block.index("rm -rf /tmp/sandbox-wheels")
        )


if __name__ == "__main__":
    unittest.main()
