# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import base64
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import unittest

from contracts import ACTIVE_LOCK_FILES, BASELINE_SNAPSHOTS, LOCK_FILES, check_modules

ROOT = Path(__file__).resolve().parents[1]
GENERATED = ROOT / "locks/generated"
spec = importlib.util.spec_from_file_location("baseline_import_locks", ROOT / "scripts/import-locks.py")
import_locks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(import_locks)

# Captured from the verified chhp artifact before the path-only migration.
# A future dependency update requires a newly reviewed artifact, not rewriting
# these historical digests as part of a representation-only change.
CHHP_MANIFEST = """4a7bed6907c8f8918791e69539597405d6ad18f3f386788a81ed28f024113ac8  ./api-modules.json
e304bf72c1bf687cf777af1ca1c70d96e928a86bfb40bc6d5aeb8239baac24f6  ./api/v2/go.mod
3410f76108a083dc3a603327c354639a078ee8d917146aa7bf769e37cce0f208  ./api/v2/go.sum
7aab541b65a7246f95a919912582d909a8cfc09e9348a10e34afac1773c29644  ./dependencies.patch
34b7b1e48a6020a855741fc497f0782f226fa75a701cf925c52ccdd322d38046  ./go.mod
6e47d5b5c5cd6ff030ffe276d8707e41c67e88321a6bc81248f61c58271ec59b  ./go.sum
e65265ab047188c71f6c5369a5be77d8cbbbefe26e307eb505775050ebece449  ./graph.txt
d5a430f8c8fea443ebc134cff29ddc0914bdc7acc1220fce9073c439e1a780d6  ./inputs.lock
2381158f636af5715561fca4208f4d89ca22a0e9c393bd53daa984a644ce5b72  ./modules.json
23d3448085a87cb3d242e196bb8ad7b571b63392c90871c2f5b26b1c5f0b6575  ./requests.txt
7e35a947feee25f89c2649aa041942db786be35c3d6c484ab2ba2da849e3dc00  ./toolchain.txt
181fc42a509297fe685e3470388d378a229e6efd36dd874eef809b6e06fe41e8  ./upstream/api/v2/go.mod
ccf35830330ac8058a4f009babbb417039aec728cf92625572abc53474c5dfca  ./upstream/api/v2/go.sum
3390a3a2aa213fa80b7cb857ae52eb031e0f1941292f3536951092acb1db1501  ./upstream/go.mod
09fab6a9bedf5e220ee82f75f8fc7db4f52854012875082f24ba629020980d13  ./upstream/go.sum
"""


def artifact_log(files):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w:gz") as archive:
        for name, data in sorted(files.items()):
            member = tarfile.TarInfo("./" + name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    payload = stream.getvalue()
    return ("KARS_DEX_LOCKS_BASE64_BEGIN\n" + base64.b64encode(payload).decode() +
            "\nKARS_DEX_LOCKS_BASE64_END\n" + hashlib.sha256(payload).hexdigest() +
            "  /tmp/dex-locks.tar.gz\n")


class BaselineRepresentationContracts(unittest.TestCase):
    def test_migration_preserves_every_artifact_digest_and_only_changes_manifest_paths(self):
        expected_manifest = []
        for line in CHHP_MANIFEST.splitlines():
            digest, original = line.split("  ", 1)
            renamed = original + ".snapshot" if original.startswith("./upstream/") else original
            self.assertEqual(hashlib.sha256((GENERATED / renamed).read_bytes()).hexdigest(), digest, renamed)
            expected_manifest.append(f"{digest}  {renamed}\n")
        self.assertEqual((GENERATED / "SHA256SUMS").read_bytes(), "".join(expected_manifest).encode())
        check_modules(ROOT)

    def test_only_active_go_locks_keep_recognized_manifest_filenames(self):
        expected_active = {"go.mod", "go.sum", "api/v2/go.mod", "api/v2/go.sum"}
        self.assertEqual(ACTIVE_LOCK_FILES, expected_active)
        recognized = {str(path.relative_to(GENERATED)) for path in GENERATED.rglob("*")
                      if path.is_file() and path.name in ("go.mod", "go.sum")}
        self.assertEqual(recognized, expected_active)
        self.assertEqual(BASELINE_SNAPSHOTS, {f"upstream/{name}.snapshot" for name in expected_active})
        for name in expected_active:
            self.assertFalse((GENERATED / "upstream" / name).exists())
            self.assertTrue((GENERATED / "upstream" / (name + ".snapshot")).is_file())

    def test_new_artifacts_import_without_altering_active_or_archival_bytes(self):
        files = {name: (GENERATED / name).read_bytes() for name in LOCK_FILES}
        imported = import_locks.decode_artifact(artifact_log(files))
        self.assertEqual(set(imported), LOCK_FILES)
        for name, data in files.items():
            self.assertEqual(imported[name], data, name)

    def test_import_rejects_legacy_or_duplicate_active_looking_baselines(self):
        files = {name: (GENERATED / name).read_bytes() for name in LOCK_FILES}
        legacy = {name.removesuffix(".snapshot") if name in BASELINE_SNAPSHOTS else name: data
                  for name, data in files.items()}
        legacy["SHA256SUMS"] = CHHP_MANIFEST.encode()
        mixed = {**files, "upstream/go.mod": files["upstream/go.mod.snapshot"]}
        for invalid in (legacy, mixed):
            with self.assertRaisesRegex(ValueError, "unexpected or duplicate lock member"):
                import_locks.decode_artifact(artifact_log(invalid))

    def test_generator_validator_and_runtime_metadata_use_the_data_layout(self):
        fetch = (ROOT / "scripts/fetch-source.sh").read_text()
        self.assertIn('cp "$file" "/packaging/upstream/$file.snapshot"', fetch)
        generator = (ROOT / "scripts/generate-locks.sh").read_text()
        self.assertIn('cp -R /packaging/upstream /out/upstream', generator)
        self.assertIn('"/packaging/upstream/$file.snapshot" "/out/$file"', generator)
        self.assertIn('--label "upstream/$file" --label "kars/$file"', generator)
        verifier = (ROOT / "scripts/verify-locks.sh").read_text()
        self.assertIn('cmp "/packaging/upstream/$file.snapshot" "/locks/upstream/$file.snapshot"', verifier)
        self.assertIn('cp "/locks/$file" "$file"', verifier)
        self.assertNotIn('cp "/locks/upstream/', verifier)
        self.assertIn('cp -R /locks /out/doc/locks', (ROOT / "scripts/build.sh").read_text())
        self.assertIn('COPY --from=build /out/doc/ /usr/share/doc/dex/', (ROOT / "Dockerfile").read_text())


if __name__ == "__main__":
    unittest.main()
