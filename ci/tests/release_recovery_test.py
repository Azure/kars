# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import copy
from datetime import datetime, timedelta, timezone
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch, Mock
import zipfile

SPEC = importlib.util.spec_from_file_location(
    "release_recovery", Path(__file__).resolve().parents[1] / "release_recovery.py"
)
RECOVERY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RECOVERY)
ROOT = Path(__file__).resolve().parents[2]
VERSION = "v0.2.0-beta.6"
SOURCE = "a" * 40
CURRENT = "b" * 40
REPO = "Azure/kars"
RUN_ID = 123


def zip_bytes(entries):
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, content in entries:
            archive.writestr(name, content)
    return buffer.getvalue()


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix=".release-recovery-test-", dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        env = patch.dict(os.environ, {
            "GITHUB_REPOSITORY": REPO, "GITHUB_SHA": CURRENT,
            "GITHUB_REF": "refs/heads/kars-bridge", "GITHUB_OUTPUT": str(self.root / "outputs"),
        })
        env.start()
        self.addCleanup(env.stop)
        now = datetime.now(timezone.utc).replace(microsecond=0)
        self.start = (now - timedelta(hours=2)).isoformat()
        self.created = (now - timedelta(hours=1)).isoformat()
        self.end = (now - timedelta(minutes=30)).isoformat()
        self.expiry = (now + timedelta(days=30)).isoformat()
        self.run = {
            "id": RUN_ID, "repository": {"id": 42, "full_name": REPO},
            "head_repository": {"id": 42, "full_name": REPO},
            "path": RECOVERY.WORKFLOW, "event": "workflow_dispatch", "head_branch": "kars-bridge",
            "head_sha": SOURCE, "status": "completed", "conclusion": "failure", "run_attempt": 1,
            "created_at": self.start, "updated_at": self.end,
        }
        names, artifact_specs = RECOVERY.expected_producers(VERSION)
        self.jobs = []
        for index, name in enumerate(sorted(names | {RECOVERY.PACKAGING, RECOVERY.PUBLICATION}), 1):
            self.jobs.append({
                "id": index, "name": name, "run_id": RUN_ID, "run_attempt": 1, "head_sha": SOURCE,
                "status": "completed", "conclusion": (
                    "failure" if name == RECOVERY.PACKAGING else
                    "skipped" if name == RECOVERY.PUBLICATION else "success"),
                "started_at": self.start, "completed_at": self.end,
            })
        self.artifacts = []
        for index, name in enumerate(sorted(artifact_specs), 1000):
            self.artifacts.append({
                "id": index, "name": name, "size_in_bytes": 100, "expired": False,
                "digest": "sha256:" + "c" * 64, "created_at": self.created,
                "updated_at": self.created, "expires_at": self.expiry,
                "workflow_run": {"id": RUN_ID, "repository_id": 42, "head_repository_id": 42,
                                 "head_branch": "kars-bridge", "head_sha": SOURCE},
            })
        self.calls = []
        api = patch.object(RECOVERY, "gh_json", side_effect=self.api)
        api.start()
        self.addCleanup(api.stop)

    def api(self, endpoint):
        self.calls.append(endpoint)
        if endpoint == f"repos/{REPO}/branches/kars-bridge":
            return {"name": "kars-bridge", "protected": True, "commit": {"sha": CURRENT}}
        base = f"repos/{REPO}/actions/runs/{RUN_ID}"
        if endpoint == base:
            return copy.deepcopy(self.run)
        for resource, key, entries in (("attempts/1/jobs", "jobs", self.jobs),
                                       ("artifacts", "artifacts", self.artifacts)):
            prefix = f"{base}/{resource}?per_page=100&page="
            if endpoint.startswith(prefix):
                offset = (int(endpoint[len(prefix):]) - 1) * 100
                return {"total_count": len(entries), key: copy.deepcopy(entries[offset:offset + 100])}
        raise AssertionError("Unexpected API endpoint: " + endpoint)

    def inventory(self):
        return RECOVERY.inventory(RUN_ID, VERSION, REPO)

    def manifest(self):
        producer, artifacts, jobs = self.inventory()
        return {"schema_version": 1, "version": VERSION, "producer": producer,
                "packaging": {"repository": REPO, "source_sha": CURRENT},
                "artifacts": artifacts, "jobs": jobs}

    def git(self, args, **kwargs):
        if args == ["git", "rev-parse", "HEAD"]:
            return Mock(stdout=CURRENT + "\n")
        if args[:3] == ["git", "merge-base", "--is-ancestor"]:
            return Mock(stdout="")
        if args[:3] == ["git", "diff", "--name-status"]:
            return Mock(stdout=getattr(self, "diff", ""))
        raise AssertionError("Unexpected git command")

    def test_complete_inventory_preserves_original_ids_source_attempt_and_digest(self):
        producer, artifacts, jobs = self.inventory()
        self.assertEqual(len(jobs), 55)
        self.assertEqual(len(artifacts), 72)
        self.assertEqual(producer["source_sha"], SOURCE)
        self.assertEqual(producer["run_attempt"], 1)
        self.assertEqual({a["id"] for a in artifacts}, {a["id"] for a in self.artifacts})
        self.assertTrue(all(a["digest"].startswith("sha256:") for a in artifacts))
        self.assertEqual([sum(a["kind"] == kind for a in artifacts)
                          for kind in ("images", "sboms", "binaries")], [32, 39, 1])

    def test_prepare_outputs_one_line_manifest_without_downloads(self):
        self.diff = "M\0.github/workflows/release-public-interim.yml\0A\0ci/release_recovery.py\0"
        with patch.object(RECOVERY.subprocess, "run", side_effect=self.git), \
                patch.object(RECOVERY, "download_archive") as download:
            manifest = RECOVERY.prepare(RUN_ID, VERSION)
        lines = (self.root / "outputs").read_text().splitlines()
        self.assertEqual(lines[:3], [f"run_id={RUN_ID}", f"source_sha={SOURCE}", "recovery=true"])
        self.assertEqual(json.loads(lines[3].removeprefix("manifest=")), manifest)
        download.assert_not_called()
        self.assertFalse(any(endpoint.endswith("/zip") for endpoint in self.calls))

    def test_wrong_run_identity_state_workflow_or_ref_fails(self):
        changes = [
            {"id": RUN_ID + 1}, {"repository": {"id": 42, "full_name": "other/repo"}},
            {"head_repository": {"id": 43, "full_name": REPO}},
            {"head_repository": {"id": 42, "full_name": "fork/kars"}},
            {"path": ".github/workflows/other.yml"}, {"head_branch": "other"},
            {"event": "pull_request"}, {"status": "in_progress"}, {"conclusion": "success"},
            {"run_attempt": 0}, {"head_sha": "not-a-sha"},
        ]
        for change in changes:
            with self.subTest(change=change), self.assertRaises(ValueError):
                RECOVERY.validate_run(self.run | change, RUN_ID, REPO)

    def test_invalid_version_rejected(self):
        for version in ("0.2.0", "v01.2.0", "v1.2.3-01", "v1.2.3\n", None):
            with self.subTest(version=version), self.assertRaises(ValueError):
                RECOVERY.version_tag(version)
        self.assertEqual(RECOVERY.version_tag("v1.2.3-beta.0+build.12"), "v1.2.3-beta.0+build.12")

    def test_missing_failed_or_wrong_attempt_producer_fails(self):
        original = copy.deepcopy(self.jobs)
        binary_index = next(i for i, job in enumerate(self.jobs) if job["name"] == RECOVERY.BINARIES)
        for changes in ({"conclusion": "failure"}, {"conclusion": "skipped"}, {"run_attempt": 2},
                        {"head_sha": CURRENT}, {"run_id": 456}, {"status": "in_progress"}):
            self.jobs = copy.deepcopy(original)
            self.jobs[binary_index].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.inventory()
        self.jobs = [job for job in original if job["name"] != RECOVERY.WHEELS]
        with self.assertRaisesRegex(ValueError, "Missing required producer"):
            self.inventory()

    def test_packaging_must_fail_and_release_must_be_skipped(self):
        for name in (RECOVERY.PACKAGING, RECOVERY.PUBLICATION):
            job = next(job for job in self.jobs if job["name"] == name)
            original = job["conclusion"]
            job["conclusion"] = "success"
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.inventory()
            job["conclusion"] = original

    def test_unexpected_failed_job_not_ignored(self):
        self.jobs.append(self.jobs[0] | {"id": 999, "name": "Additional gate", "conclusion": "failure"})
        with self.assertRaisesRegex(ValueError, "Additional gate"):
            self.inventory()

    def test_wrong_version_expiry_digest_size_binding_or_attempt_fails(self):
        original = copy.deepcopy(self.artifacts[0])
        changes = [
            {"name": original["name"].replace(VERSION, "v0.2.0-beta.5")},
            {"expired": True}, {"expires_at": self.start}, {"digest": None},
            {"digest": "sha256:bad"}, {"size_in_bytes": 0}, {"size_in_bytes": 300 * RECOVERY.MIB},
            {"workflow_run": original["workflow_run"] | {"head_sha": CURRENT}},
            {"workflow_run": original["workflow_run"] | {"head_repository_id": 43}},
            {"workflow_run": original["workflow_run"] | {"id": 124}},
            {"created_at": "2000-01-01T00:00:00Z"},
        ]
        for change in changes:
            self.artifacts[0] = original | change
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.inventory()

    def test_missing_and_duplicate_artifacts_fail(self):
        removed = self.artifacts.pop()
        with self.assertRaisesRegex(ValueError, "Missing required"):
            self.inventory()
        self.artifacts.append(removed)
        self.artifacts.append(removed | {"id": 9999})
        with self.assertRaisesRegex(ValueError, "Duplicate artifact name"):
            self.inventory()

    def test_nonselected_artifacts_do_not_require_digest(self):
        self.artifacts.append({"id": 999, "name": "docker-build-record"})
        self.assertEqual(len(self.inventory()[1]), 72)

    def test_full_job_and_artifact_pagination(self):
        for index in range(180):
            self.jobs.append(self.jobs[0] | {"id": 3000 + index, "name": f"Additional gate {index}"})
            self.artifacts.append({"id": 3000 + index, "name": f"build-record-{index}"})
        _, artifacts, jobs = self.inventory()
        self.assertEqual(len(jobs), 235)
        self.assertEqual(len(artifacts), 72)
        self.assertIn(f"repos/{REPO}/actions/runs/{RUN_ID}/attempts/1/jobs?per_page=100&page=3", self.calls)
        self.assertIn(f"repos/{REPO}/actions/runs/{RUN_ID}/artifacts?per_page=100&page=3", self.calls)

    def test_truncated_duplicate_or_changing_pagination_fails(self):
        cases = [
            [{"total_count": 2, "jobs": [{"id": 1}]}, {"total_count": 2, "jobs": []}],
            [{"total_count": 2, "jobs": [{"id": 1}, {"id": 1}]}],
            [{"total_count": 2, "jobs": [{"id": 1}]}, {"total_count": 1, "jobs": [{"id": 2}]}],
        ]
        for responses in cases:
            with self.subTest(responses=responses), patch.object(RECOVERY, "gh_json", side_effect=responses), \
                    self.assertRaises(ValueError):
                RECOVERY.pages("endpoint", "jobs")

    def test_run_rerun_race_fails_closed(self):
        initial = self.api
        def changing(endpoint):
            result = initial(endpoint)
            if endpoint.endswith(f"/runs/{RUN_ID}") and self.calls.count(endpoint) == 2:
                result["run_attempt"] = 2
            return result
        with patch.object(RECOVERY, "gh_json", side_effect=changing), \
                self.assertRaisesRegex(ValueError, "changed during validation"):
            self.inventory()

    def test_auth_errors_remain_errors(self):
        with patch.object(RECOVERY, "gh_json", side_effect=subprocess.CalledProcessError(1, ["gh", "api"])), \
                self.assertRaises(subprocess.CalledProcessError):
            self.inventory()

    def test_allowlist_accepts_only_packaging_and_markdown_audit_additions_modifications(self):
        producer = self.inventory()[0]
        for path in sorted(RECOVERY.ALLOWLIST | {"docs/security-audits/new/audit.md"}):
            self.diff = f"M\0{path}\0"
            with self.subTest(path=path), patch.object(RECOVERY.subprocess, "run", side_effect=self.git):
                RECOVERY.validate_source(producer, REPO, CURRENT)
        for diff in ("M\0controller/src/main.rs\0", "M\0cli/scripts/release-artifacts.mjs\0",
                     "M\0.github/workflows/ci.yml\0", "M\0docs/security-audits/check.py\0",
                     "D\0ci/release_recovery.py\0", "T\0ci/release_recovery.py\0",
                     "R100\0old\0ci/release_recovery.py\0"):
            self.diff = diff
            with self.subTest(diff=diff), patch.object(RECOVERY.subprocess, "run", side_effect=self.git), \
                    self.assertRaises(ValueError):
                RECOVERY.validate_source(producer, REPO, CURRENT)

    def test_unprotected_branch_wrong_checkout_and_nonancestor_fail(self):
        producer = self.inventory()[0]
        with patch.object(RECOVERY, "gh_json", return_value={"name": "kars-bridge", "protected": False}), \
                self.assertRaisesRegex(ValueError, "not protected"):
            RECOVERY.validate_source(producer, REPO, CURRENT)
        with patch.object(RECOVERY.subprocess, "run", return_value=Mock(stdout=SOURCE)), \
                self.assertRaisesRegex(ValueError, "Checkout"):
            RECOVERY.validate_source(producer, REPO, CURRENT)
        def nonancestor(args, **kwargs):
            if args[1] == "merge-base":
                raise subprocess.CalledProcessError(1, args)
            return self.git(args, **kwargs)
        with patch.object(RECOVERY.subprocess, "run", side_effect=nonancestor), \
                self.assertRaises(subprocess.CalledProcessError):
            RECOVERY.validate_source(producer, REPO, CURRENT)
        with patch.dict(os.environ, {"GITHUB_REF": "refs/tags/v0.2.0-beta.6"}), \
                self.assertRaisesRegex(ValueError, "protected.*ref"):
            RECOVERY.context()

    def test_download_revalidates_manifest_and_downloads_immutable_binary_id(self):
        data = zip_bytes([("amd64/kars-controller", b"real archived binary bytes")])
        binary = next(a for a in self.artifacts if a["name"].startswith("kars-binaries-"))
        binary.update(size_in_bytes=len(data), digest="sha256:" + hashlib.sha256(data).hexdigest())
        manifest = self.manifest()
        process = Mock(stdout=io.BytesIO(data))
        process.wait.return_value = 0
        process.poll.return_value = 0
        with patch.dict(os.environ, {"RELEASE_RECOVERY_MANIFEST": json.dumps(manifest)}), \
                patch.object(RECOVERY.subprocess, "Popen", return_value=process) as launch:
            RECOVERY.download("binaries", self.root / "binaries")
        self.assertEqual(launch.call_args.args[0],
                         ["gh", "api", f"repos/{REPO}/actions/artifacts/{binary['id']}/zip"])
        self.assertEqual((self.root / "binaries/amd64/kars-controller").read_bytes(), b"real archived binary bytes")
        self.assertEqual(list((self.root / "binaries").glob(".release-recovery-*")), [])

    def test_manifest_metadata_and_attempt_drift_fails_before_download(self):
        original = self.manifest()
        changes = [
            lambda m: m["producer"].update(run_attempt=2),
            lambda m: m["producer"].update(source_sha=CURRENT),
            lambda m: m["packaging"].update(repository="other/repo"),
            lambda m: m["artifacts"][0].update(digest="sha256:" + "d" * 64),
            lambda m: m["artifacts"][0].update(id=99999),
            lambda m: m["artifacts"].pop(),
        ]
        for change in changes:
            manifest = copy.deepcopy(original)
            change(manifest)
            with patch.dict(os.environ, {"RELEASE_RECOVERY_MANIFEST": json.dumps(manifest)}), \
                    patch.object(RECOVERY, "download_archive") as transfer, self.assertRaises(ValueError):
                RECOVERY.download("images", self.root / "images")
            transfer.assert_not_called()

    def test_failed_download_leaves_no_archives_or_partial_extraction(self):
        manifest = self.manifest()
        def failed_transfer(repo, artifact, archive):
            archive.write_bytes(b"unverified bytes")
            raise ValueError("Downloaded ZIP digest mismatch")
        with patch.dict(os.environ, {"RELEASE_RECOVERY_MANIFEST": json.dumps(manifest)}), \
                patch.object(RECOVERY, "download_archive", side_effect=failed_transfer), \
                self.assertRaisesRegex(ValueError, "digest mismatch"):
            RECOVERY.download("images", self.root / "images")
        self.assertEqual(list((self.root / "images").iterdir()), [])

    def test_invalid_manifest_run_id_never_reaches_network(self):
        for run_id in (None, "123", "123/other", -1, True):
            with self.subTest(run_id=run_id), patch.object(RECOVERY, "gh_json") as api, \
                    self.assertRaisesRegex(ValueError, "Invalid original run ID"):
                RECOVERY.inventory(run_id, VERSION, REPO)
            api.assert_not_called()

    def test_download_digest_size_and_auth_failure(self):
        data = zip_bytes([("receipt.json", b"{}")])
        valid = {"id": 42, "size_in_bytes": len(data), "digest": "sha256:" + hashlib.sha256(data).hexdigest()}
        for index, (artifact, body, status) in enumerate((
            (valid | {"digest": "sha256:" + "0" * 64}, data, 0),
            (valid, data + b"extra", 0), (valid, data[:-1], 0), (valid, data, 1),
        )):
            process = Mock(stdout=io.BytesIO(body))
            process.wait.return_value = status
            process.poll.return_value = status
            with patch.object(RECOVERY.subprocess, "Popen", return_value=process), self.assertRaises(ValueError):
                RECOVERY.download_archive(REPO, artifact, self.root / f"archive-{index}.zip")


class ExtractionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix=".release-recovery-zip-test-", dir=ROOT)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.archive = self.root / "artifact.zip"
        self.destination = self.root / "out"

    def extract(self, entries, kind="images", remaining=RECOVERY.MAX_TOTAL_EXPANDED):
        self.archive.write_bytes(zip_bytes(entries))
        return RECOVERY.extract_zip(self.archive, self.destination, kind, remaining)

    def test_extract_files_directories_and_identical_overlap(self):
        entries = [("receipts/", b""), ("receipts/image.json", b'{"digest":"sha256:123"}')]
        self.assertGreater(self.extract(entries), 0)
        self.extract(entries)
        self.assertEqual((self.destination / "receipts/image.json").read_bytes(), entries[1][1])

    def test_reject_paths_escaping_or_ambiguous(self):
        for path in ("../escaped", "/absolute", "a/../../escaped", "a//file", "./file",
                     "a\\file", "C:/file", "a/../file", "line\nbreak"):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "Unsafe ZIP path"):
                self.extract([(path, b"bad")])
        self.assertFalse((self.root / "escaped").exists())

    def test_reject_symlinks_and_special_entries(self):
        for mode in (stat.S_IFLNK, stat.S_IFIFO, stat.S_IFCHR, stat.S_IFBLK, stat.S_IFSOCK):
            info = zipfile.ZipInfo("danger")
            info.create_system = 3
            info.external_attr = (mode | 0o777) << 16
            with self.subTest(mode=mode), self.assertRaisesRegex(ValueError, "link or special"):
                self.extract([(info, b"../escaped")])

    def test_reject_existing_symlink_destination_or_parent(self):
        self.destination.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "Symlink"):
            self.extract([("file", b"data")])
        self.destination.unlink()
        self.destination.mkdir()
        (self.destination / "link").symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "Symlink"):
            self.extract([("link/file", b"data")])

    def test_collision_never_overwrites_different_bytes(self):
        self.extract([("receipt.json", b"first")])
        with self.assertRaisesRegex(ValueError, "overwrite different content"):
            self.extract([("receipt.json", b"second")])
        self.assertEqual((self.destination / "receipt.json").read_bytes(), b"first")
        self.assertEqual(list(self.destination.glob(".recovery-member-*")), [])

    def test_entry_count_expansion_ratio_and_aggregate_bounds(self):
        with self.assertRaisesRegex(ValueError, "entry count"):
            self.extract([(str(index), b"x") for index in range(129)])
        with self.assertRaisesRegex(ValueError, "expansion ratio"):
            self.extract([("bomb", b"0" * 1000000)])
        with self.assertRaisesRegex(ValueError, "expanded size"):
            self.extract([("small", b"12345")], remaining=4)
        with patch.dict(RECOVERY.LIMITS, {"images": (1024, 4, 128)}), \
                self.assertRaisesRegex(ValueError, "expanded size"):
            self.extract([("small", b"12345")])

    def test_disk_headroom_required(self):
        with patch.object(RECOVERY.shutil, "disk_usage", return_value=Mock(free=0)), \
                self.assertRaisesRegex(ValueError, "disk headroom"):
            self.extract([("file", b"data")])

    def test_duplicate_and_file_directory_collisions(self):
        with self.assertWarns(UserWarning), self.assertRaisesRegex(ValueError, "Duplicate ZIP path"):
            self.extract([("same", b"first"), ("same", b"second")])
        self.extract([("file", b"data")])
        with self.assertRaisesRegex(ValueError, "Non-directory"):
            self.extract([("file/child", b"data")])

    def test_corrupt_archive_fails(self):
        self.archive.write_bytes(b"not a ZIP")
        with self.assertRaises(zipfile.BadZipFile):
            RECOVERY.extract_zip(self.archive, self.destination, "images")


if __name__ == "__main__":
    unittest.main()
