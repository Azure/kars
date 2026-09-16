# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import ast
import base64
import gzip
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import unittest

from contracts import BASE, LOCK_FILES, RPM_MANIFEST, check_modules, check_rootfs, check_scan, json_stream, module_inventory

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("import_locks", ROOT / "scripts/import-locks.py")
import_locks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(import_locks)


class SourceContracts(unittest.TestCase):
    def test_json_stream(self):
        self.assertEqual(list(json_stream('{"Path":"a"}\n{"Path":"b"}\n')),
                         [{"Path": "a"}, {"Path": "b"}])

    def test_module_inventory_rejects_resolver_errors_and_duplicates(self):
        self.assertEqual(module_inventory('{"Path":"module-a","Version":"v1.0.0"}'),
                         {"module-a": {"Path": "module-a", "Version": "v1.0.0"}})
        for bad in ('{"Path":"a"}\n{"Path":"a"}', '{"Path":"a","Error":{"Err":"unresolved"}}',
                    '{"Version":"v1.0.0"}', '[]'):
            with self.assertRaises(ValueError):
                module_inventory(bad)

    def test_runtime_recipe(self):
        dockerfile = (ROOT / "Dockerfile").read_text()
        runtime = dockerfile.split(f"FROM {BASE} AS runtime\n")[1]
        self.assertNotIn("\nRUN ", runtime)
        self.assertNotIn("--from=dependencies", runtime)
        self.assertNotIn("COPY .", runtime)
        self.assertIn('ENTRYPOINT ["/usr/local/bin/dex"]', runtime)
        self.assertIn('CMD ["serve", "/etc/dex/config.yaml"]', runtime)
        self.assertIn("COPY locks/generated/ /locks/", dockerfile)
        self.assertIn("CGO_ENABLED=1", dockerfile)
        self.assertIn("GOTOOLCHAIN=local", dockerfile)
        self.assertNotIn("go get", (ROOT / "scripts/build.sh").read_text())

    def test_resolver_is_separate_from_runtime_build(self):
        runtime = (ROOT / "Dockerfile").read_text()
        maintenance = (ROOT / "Dockerfile.locks").read_text()
        self.assertNotIn("generate-locks", runtime)
        self.assertNotIn("FROM lock-generation", runtime)
        self.assertNotIn("lock-replay", runtime)
        self.assertIn("FROM lock-generation AS lock-replay", maintenance)
        self.assertIn("cmp SHA256SUMS /out/SHA256SUMS", maintenance)
        def source(text):
            return text[text.index("FROM golang:"):text.index("\nFROM source AS ")]

        self.assertEqual(source(runtime), source(maintenance))
        self.assertIn("!Dockerfile.locks", (ROOT / ".dockerignore").read_text())

    def test_missing_locks_block_acceptance(self):
        if (ROOT / "locks/generated").is_dir():
            check_modules(ROOT)
        else:
            with self.assertRaisesRegex(ValueError, "runtime build is blocked"):
                check_modules(ROOT)

    def test_notice_collector_preserves_british_spelled_upstream_licences(self):
        source = (ROOT / "scripts/notices.go").read_text()
        self.assertEqual(source.count('strings.HasPrefix(name, "LICENCE")'), 2)
        self.assertIn('strings.ToUpper(entry.Name())', source)
        self.assertIn('missing root license; review upstream attribution before shipping', source)
        self.assertIn('os.WriteFile(dest, data, 0644)', source)

    def test_base_inventory_must_survive(self):
        base = {RPM_MANIFEST: ("file", 420, "manifest"),
                "etc/pki/ca-trust/bundle.pem": ("file", 420, "trust")}
        runtime = {**base, "usr/local/bin/dex": ("file", 493, "dex")}
        manifest = "\n".join(f"rpm-{i}" for i in range(14))
        check_rootfs(base, runtime, manifest)
        for name in base:
            altered = dict(runtime)
            altered[name] = ("file", 420, "changed")
            with self.assertRaisesRegex(ValueError, "base file modified"):
                check_rootfs(base, altered, manifest)
        with self.assertRaisesRegex(ValueError, "14 Azure Linux RPM"):
            check_rootfs(base, runtime, manifest + "\nunreviewed-rpm")
        with self.assertRaisesRegex(ValueError, "unexpected runtime payload"):
            check_rootfs(base, {**runtime, "usr/lib/libc.so": ("file", 493, "debian")}, manifest)

    def test_scanner_must_see_both_os_and_go(self):
        report = {
            "Metadata": {"OS": {"Family": "azurelinux"}},
            "Results": [
                {"Class": "os-pkgs", "Packages": [{"Name": f"rpm-{i}"} for i in range(14)]},
                {"Type": "gobinary", "Target": "/usr/local/bin/dex"},
            ],
        }
        check_scan(report)
        report["Results"][1]["Vulnerabilities"] = [{"Severity": "HIGH"}]
        with self.assertRaisesRegex(ValueError, "findings remain"):
            check_scan(report)
        report["Results"].pop()
        with self.assertRaisesRegex(ValueError, "Dex Go binary"):
            check_scan(report)

    def test_python_syntax(self):
        for file in ROOT.rglob("*.py"):
            ast.parse(file.read_text(), filename=str(file))

    def test_lock_archive_boundaries(self):
        def log_for(names, checksum=None):
            stream = io.BytesIO()
            with tarfile.open(fileobj=stream, mode="w:gz") as archive:
                for name in names:
                    member = tarfile.TarInfo("./" + name)
                    member.size = 4
                    archive.addfile(member, io.BytesIO(b"test"))
            payload = stream.getvalue()
            checksum = checksum or hashlib.sha256(payload).hexdigest()
            return ("KARS_DEX_LOCKS_BASE64_BEGIN\n" +
                    base64.b64encode(payload).decode() +
                    "\nKARS_DEX_LOCKS_BASE64_END\n" +
                    checksum + "  /tmp/dex-locks.tar.gz\n")

        names = sorted(LOCK_FILES)
        self.assertEqual(set(import_locks.decode_artifact(log_for(names))), set(names))
        for invalid in (names + ["go.mod"], names + ["../escape"], names[:-1]):
            with self.assertRaises(ValueError):
                import_locks.decode_artifact(log_for(invalid))
        with self.assertRaisesRegex(ValueError, "exactly one"):
            import_locks.decode_artifact(log_for(names) + log_for(names))
        with self.assertRaisesRegex(ValueError, "transport SHA-256"):
            import_locks.decode_artifact(log_for(names, "0" * 64))
        with self.assertRaisesRegex(ValueError, "transport SHA-256"):
            import_locks.decode_artifact(log_for(names).replace("/tmp/dex-locks.tar.gz", "/tmp/wrong.tar.gz"))

    def test_archive_limit_includes_headers_and_padding(self):
        payload = gzip.compress(b"\0" * (import_locks.MAX_ARCHIVE_BYTES + 1))
        log = ("KARS_DEX_LOCKS_BASE64_BEGIN\n" + base64.b64encode(payload).decode() +
               "\nKARS_DEX_LOCKS_BASE64_END\n" + hashlib.sha256(payload).hexdigest() +
               "  /tmp/dex-locks.tar.gz\n")
        with self.assertRaisesRegex(ValueError, "expanded artifact exceeds"):
            import_locks.decode_artifact(log)


if __name__ == "__main__":
    unittest.main()
