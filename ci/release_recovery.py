#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Recover retained public-release producers without rebuilding or relabelling them."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile

WORKFLOW = ".github/workflows/release-public-interim.yml"
BRANCH = "kars-bridge"
PACKAGING = "Package release-bound charts and CLI; attempt npm OIDC publication"
PUBLICATION = "Create PUBLIC GitHub Release"
PREPARE = "Validate release version and source"
BINARIES = "Build Rust binaries (amd64 + arm64)"
WHEELS = "Build AGT Python wheels (from pinned AGT)"
SANDBOXES = "Merge + sign default harness and sandbox-base manifests"
ALLOWLIST = {
    WORKFLOW, "ci/release_recovery.py", "ci/tests/release_recovery_test.py",
    "cli/scripts/release-artifacts.test.mjs", "bridge/docs/public-beta-helm.md",
    "ci/security-audit-required.sh", "ci/tests/security_audit_gate_test.py",
    "docs/security-audits/README.md",
}
IMAGES = (
    "controller", "metrics-server", "headlamp", "inference-router", "a2a-gateway",
    "conformance-runner", "bridge-web", "bridge-teams-gateway", "bridge-idp",
    "schema-hook", "mission-dispatcher", "webhook-certgen", "prometheus-operator",
    "datapath-witness", "k8s-sidecar", "kube-state-metrics", "mcp-everything",
    "json-exporter", "prometheus-config-reloader", "node-exporter",
)
NATIVE_IMAGES = ("bridge-bff", "prometheus", "grafana", "mcp-playwright")
RUNTIMES = ("langgraph", "maf-python", "anthropic", "openai-agents", "pydantic-ai")
ARCHES = ("amd64", "arm64")
MIB = 1024 * 1024
# ZIP bytes, expanded bytes, member count. Binary ZIP is currently about 81 MB.
LIMITS = {"images": (2 * MIB, 8 * MIB, 128),
          "binaries": (256 * MIB, 1024 * MIB, 64),
          "sboms": (32 * MIB, 256 * MIB, 128)}
MAX_TOTAL_ZIP = 512 * MIB
MAX_TOTAL_EXPANDED = 2 * 1024 * MIB
HEADROOM = 128 * MIB
MAX_MANIFEST_BYTES = 120000  # Also fits Linux's per-environment-variable limit.
SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def timestamp(value):
    require(isinstance(value, str), "Missing timestamp")
    result = datetime.fromisoformat(value.replace("Z", "+00:00"))
    require(result.tzinfo is not None, "Timestamp must include timezone")
    return result


def version_tag(value):
    require(isinstance(value, str) and re.fullmatch(
        r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
        r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
        r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?", value),
        "Version must be canonical v-prefixed SemVer")
    prerelease = value.split("+", 1)[0].partition("-")[2]
    require(all(not part.isdigit() or part == "0" or not part.startswith("0")
                for part in prerelease.split(".")), "Noncanonical numeric prerelease")
    return value


def gh_json(endpoint):
    result = subprocess.run(["gh", "api", endpoint], check=True, capture_output=True, text=True)
    return json.loads(result.stdout)


def pages(endpoint, key):
    """Require the API's full count; never silently accept a truncated job/artifact list."""
    entries, expected, page = [], None, 1
    while True:
        data = gh_json(f"{endpoint}?per_page=100&page={page}")
        total, batch = data.get("total_count"), data.get(key)
        require(type(total) is int and 0 <= total <= 10000 and isinstance(batch, list),
                "Malformed paginated API response")
        if expected is None:
            expected = total
        require(expected == total, "API collection changed while paginating")
        entries.extend(batch)
        require(len(entries) <= total, "API pagination exceeds total_count")
        if len(entries) == total:
            break
        require(batch and len(batch) <= 100, "Truncated API pagination")
        page += 1
    ids = [entry.get("id") for entry in entries]
    require(all(type(i) is int and i > 0 for i in ids) and len(set(ids)) == len(ids),
            "Duplicate or invalid paginated IDs")
    return entries


def context():
    repo, sha = os.environ.get("GITHUB_REPOSITORY", ""), os.environ.get("GITHUB_SHA", "")
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo), "Invalid GITHUB_REPOSITORY")
    require(SHA.fullmatch(sha), "Invalid GITHUB_SHA")
    require(os.environ.get("GITHUB_REF") == f"refs/heads/{BRANCH}",
            "Recovery must execute on the protected kars-bridge ref")
    return repo, sha


def expected_producers(version):
    jobs = {PREPARE, BINARIES, WHEELS, SANDBOXES}
    artifacts = {f"kars-binaries-{version}": ("binaries", BINARIES),
                 f"image-sandboxes-{version}": ("images", SANDBOXES)}
    for name in ("openclaw-sandbox", "kars-sandbox-base"):
        artifacts[f"sbom-{name}-{version}"] = ("sboms", SANDBOXES)
    for name in IMAGES:
        job = f"Build + push {name}  (PUBLIC GHCR)"
        jobs.add(job)
        artifacts[f"image-kars-{name}-{version}"] = ("images", job)
        artifacts[f"sbom-{name}-{version}"] = ("sboms", job)
    for name in NATIVE_IMAGES:
        merge = f"Merge + sign kars-{name} native manifests"
        jobs.add(merge)
        artifacts[f"image-kars-{name}-{version}"] = ("images", merge)
        for arch in ARCHES:
            job = f"Build + push {name} {arch} (PUBLIC GHCR)"
            jobs.add(job)
            artifacts[f"sbom-{name}-{version}-{arch}"] = ("sboms", job)
    for name in (*RUNTIMES, "hermes"):
        for arch in ARCHES if name == "hermes" else ("amd64",):
            job = f"Build + push kars-runtime-{name} ({arch}, PUBLIC GHCR)"
            jobs.add(job)
            suffix = f"-{arch}" if name == "hermes" else ""
            artifacts[f"sbom-kars-runtime-{name}-{version}{suffix}"] = ("sboms", job)
            if name != "hermes":
                artifacts[f"image-kars-runtime-{name}-{version}"] = ("images", job)
    for component in ("relay", "registry"):
        merge = f"Merge + sign agentmesh manifests ({component})"
        jobs.add(merge)
        artifacts[f"image-kars-agentmesh-{component}-{version}"] = ("images", merge)
        artifacts[f"sbom-kars-agentmesh-{component}-{version}"] = ("sboms", merge)
        for arch in ARCHES:
            jobs.add(f"Build agentmesh-{component} ({arch}, native)")
    for arch in ARCHES:
        jobs.add(f"Build kars-sandbox-base ({arch}, native)")
        jobs.add(f"Build openclaw-sandbox ({arch}, native)")
    return jobs, artifacts


def validate_run(run, run_id, repo):
    require(type(run_id) is int and run_id > 0 and run.get("id") == run_id, "Wrong run ID")
    require(run.get("repository", {}).get("full_name") == repo and
            run.get("head_repository", {}).get("full_name") == repo, "Wrong producer repository")
    repository_id = run["repository"].get("id")
    require(type(repository_id) is int and repository_id > 0 and
            run["head_repository"].get("id") == repository_id, "Wrong head repository ID")
    require(run.get("path") == WORKFLOW, "Wrong producer workflow")
    require(run.get("event") == "workflow_dispatch", "Producer must be workflow_dispatch")
    require(run.get("head_branch") == BRANCH, "Wrong producer ref")
    require(run.get("status") == "completed" and run.get("conclusion") == "failure",
            "Producer run must be completed and failed")
    require(SHA.fullmatch(run.get("head_sha", "")), "Invalid producer source SHA")
    require(type(run.get("run_attempt")) is int and run["run_attempt"] > 0, "Invalid run attempt")
    timestamp(run.get("created_at"))
    timestamp(run.get("updated_at"))
    return {"repository": repo, "repository_id": repository_id, "run_id": run_id,
            "run_attempt": run["run_attempt"], "source_sha": run["head_sha"],
            "head_branch": BRANCH, "workflow": WORKFLOW, "event": run["event"],
            "created_at": run["created_at"], "updated_at": run["updated_at"]}


def validate_jobs(jobs, producer, version):
    expected, _ = expected_producers(version)
    by_name = {}
    for job in jobs:
        name = job.get("name")
        require(isinstance(name, str) and name not in by_name, "Duplicate or invalid job name")
        require(job.get("run_id") == producer["run_id"] and
                job.get("run_attempt") == producer["run_attempt"], "Wrong job run/attempt")
        require(job.get("head_sha") == producer["source_sha"], "Wrong job source")
        conclusion = "failure" if name == PACKAGING else "skipped" if name == PUBLICATION else "success"
        require(job.get("status") == "completed" and job.get("conclusion") == conclusion,
                f"Job not in required completed {conclusion} state: {name}")
        require(timestamp(job.get("started_at")) <= timestamp(job.get("completed_at")),
                "Invalid producer job time interval")
        by_name[name] = job
    require(expected | {PACKAGING, PUBLICATION} <= by_name.keys(), "Missing required producer jobs")
    return by_name


def validate_source(producer, repo, current_sha):
    branch = gh_json(f"repos/{repo}/branches/{BRANCH}")
    require(branch.get("name") == BRANCH and branch.get("protected") is True,
            "Producer source branch is not protected")
    protected_sha = branch.get("commit", {}).get("sha", "")
    require(SHA.fullmatch(protected_sha), "Invalid protected branch SHA")
    head = subprocess.run(["git", "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()
    require(head == current_sha, "Checkout does not match packaging GITHUB_SHA")
    for ancestor, descendant in ((producer["source_sha"], current_sha), (current_sha, protected_sha)):
        subprocess.run(["git", "merge-base", "--is-ancestor", ancestor, descendant], check=True,
                       capture_output=True, text=True)
    diff = subprocess.run(["git", "diff", "--name-status", "-z", "--find-renames",
                           producer["source_sha"], current_sha, "--"], check=True,
                          capture_output=True, text=True).stdout
    fields = diff.split("\0")
    if fields[-1] == "":
        fields.pop()
    require(len(fields) % 2 == 0, "Renamed or malformed source diff")
    for status_code, path in zip(fields[::2], fields[1::2]):
        require(status_code in {"A", "M"}, "Deleted, renamed or type-changed source is not recoverable")
        audit = path.startswith("docs/security-audits/") and path.endswith(".md")
        require(path in ALLOWLIST or audit, f"Product/source drift outside recovery allowlist: {path}")


def select_artifacts(artifacts, producer, jobs, version):
    _, required = expected_producers(version)
    selected, names = [], set()
    for artifact in artifacts:
        name = artifact.get("name", "")
        require(isinstance(name, str), "Malformed artifact name")
        if not name.startswith(("kars-binaries-", "image-", "sbom-")):
            continue
        require(name in required, f"Artifact version or producer does not match recovery: {name}")
        require(name not in names, f"Duplicate artifact name: {name}")
        names.add(name)
        kind, job_name = required[name]
        require(artifact.get("expired") is False and
                timestamp(artifact.get("expires_at")) > datetime.now(timezone.utc), "Artifact expired")
        require(DIGEST.fullmatch(artifact.get("digest") or ""), "Missing or invalid artifact SHA256 digest")
        size = artifact.get("size_in_bytes")
        require(type(size) is int and 0 < size <= LIMITS[kind][0], "Artifact ZIP size exceeds bound")
        require(type(artifact.get("id")) is int and artifact["id"] > 0, "Invalid artifact ID")
        binding = artifact.get("workflow_run")
        require(binding == {"id": producer["run_id"], "repository_id": producer["repository_id"],
                            "head_repository_id": producer["repository_id"], "head_branch": BRANCH,
                            "head_sha": producer["source_sha"]}, "Wrong artifact workflow/source binding")
        job = jobs[job_name]
        created, updated = timestamp(artifact.get("created_at")), timestamp(artifact.get("updated_at"))
        # GitHub artifact metadata has no attempt field. Bind creation to its exact
        # successful producer job in the current attempt, not the run-wide inventory.
        require(timestamp(job["started_at"]) <= created <= updated <= timestamp(job["completed_at"]),
                "Artifact does not belong to the selected producer job/attempt")
        selected.append({key: artifact[key] for key in (
            "id", "name", "digest", "size_in_bytes", "created_at", "updated_at", "expires_at", "workflow_run")}
            | {"kind": kind, "producer_run_attempt": producer["run_attempt"], "producer_job_id": job["id"]})
    require(names == required.keys(), "Missing required version-bound artifacts")
    require(len({a["id"] for a in selected}) == len(selected), "Duplicate selected artifact IDs")
    require(sum(a["size_in_bytes"] for a in selected) <= MAX_TOTAL_ZIP, "Total artifact ZIP size exceeds bound")
    return sorted(selected, key=lambda artifact: artifact["id"])


def inventory(run_id, version, repo):
    require(type(run_id) is int and run_id > 0, "Invalid original run ID")
    endpoint = f"repos/{repo}/actions/runs/{run_id}"
    producer = validate_run(gh_json(endpoint), run_id, repo)
    jobs = pages(f"{endpoint}/attempts/{producer['run_attempt']}/jobs", "jobs")
    by_name = validate_jobs(jobs, producer, version)
    artifacts = select_artifacts(pages(f"{endpoint}/artifacts", "artifacts"), producer, by_name, version)
    require(validate_run(gh_json(endpoint), run_id, repo) == producer,
            "Producer run changed during validation")
    evidence = [{key: job[key] for key in (
        "id", "name", "run_id", "run_attempt", "head_sha", "status", "conclusion", "started_at", "completed_at")}
        for job in sorted(jobs, key=lambda job: job["id"])]
    return producer, artifacts, evidence


def prepare(run_id, version):
    repo, current_sha = context()
    version_tag(version)
    producer, artifacts, jobs = inventory(run_id, version, repo)
    validate_source(producer, repo, current_sha)
    manifest = {"schema_version": 1, "version": version,
                "packaging": {"repository": repo, "source_sha": current_sha},
                "producer": producer, "artifacts": artifacts, "jobs": jobs}
    encoded = json.dumps(manifest, separators=(",", ":"), sort_keys=True)
    require(len(encoded.encode("utf-8")) < MAX_MANIFEST_BYTES, "Recovery manifest exceeds step output bound")
    output = os.environ.get("GITHUB_OUTPUT")
    require(output, "GITHUB_OUTPUT is required")
    with open(output, "a", encoding="utf-8") as stream:
        stream.write(f"run_id={run_id}\nsource_sha={producer['source_sha']}\nrecovery=true\nmanifest={encoded}\n")
    print(f"Validated {len(jobs) - 2} successful producer jobs and {len(artifacts)} retained artifacts "
          f"from run {run_id}, attempt {producer['run_attempt']}, source {producer['source_sha']}. "
          "No images or binaries rebuilt; no artifacts downloaded.")
    return manifest


def file_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(MIB), b""):
            digest.update(chunk)
    return digest.hexdigest()


def safe_directory(path):
    path = Path(os.path.abspath(path))
    for part in (*reversed(path.parents), path):
        require(not part.is_symlink(), "Symlink in extraction destination")
        if part.exists():
            require(part.is_dir(), "Non-directory in extraction destination")
        else:
            part.mkdir()
    return path


def extract_zip(archive, destination, kind, remaining=MAX_TOTAL_EXPANDED):
    destination = safe_directory(destination)
    _, expansion_limit, count_limit = LIMITS[kind]
    with zipfile.ZipFile(archive) as zipped:
        members = zipped.infolist()
        require(0 < len(members) <= count_limit, "ZIP entry count exceeds bound")
        total = sum(member.file_size for member in members)
        require(total <= min(expansion_limit, remaining), "ZIP expanded size exceeds bound")
        require(shutil.disk_usage(destination).free >= total + HEADROOM, "Insufficient extraction disk headroom")
        plan, paths = [], set()
        for member in members:
            name = member.filename
            parts = name.rstrip("/").split("/")
            require(name == member.orig_filename and name and not name.startswith("/") and
                    "\\" not in name and ":" not in name and
                    all(part not in {"", ".", ".."} for part in parts) and
                    not any(ord(character) < 32 or ord(character) == 127 for character in name),
                    "Unsafe ZIP path")
            mode = member.external_attr >> 16
            entry_type = stat.S_IFMT(mode)
            require(entry_type in {0, stat.S_IFREG, stat.S_IFDIR} and
                    not (entry_type == stat.S_IFDIR and not member.is_dir()) and
                    not (entry_type == stat.S_IFREG and member.is_dir()), "ZIP link or special entry")
            require(not member.flag_bits & 1 and member.compress_type in {zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED},
                    "Unsupported ZIP compression or encryption")
            require(member.file_size <= expansion_limit and
                    member.file_size <= max(1, member.compress_size) * 200, "ZIP expansion ratio exceeds bound")
            relative = PurePosixPath(*parts)
            require(relative not in paths, "Duplicate ZIP path")
            paths.add(relative)
            target = destination.joinpath(*parts)
            for parent in (target, *target.parents):
                if parent == destination:
                    break
                require(not parent.is_symlink(), "Symlink in extraction path")
                if parent != target and parent.exists():
                    require(parent.is_dir(), "Non-directory in extraction path")
            if target.exists():
                require(target.is_dir() if member.is_dir() else target.is_file(), "ZIP destination collision")
            plan.append((member, target))
        for member, target in plan:
            if member.is_dir():
                safe_directory(target)
                continue
            safe_directory(target.parent)
            # Stage under the destination, never in a system temporary directory.
            with tempfile.NamedTemporaryFile(prefix=".recovery-member-", dir=destination, delete=False) as staged:
                staged_path = Path(staged.name)
                try:
                    with zipped.open(member) as source:
                        written = 0
                        while chunk := source.read(MIB):
                            written += len(chunk)
                            require(written <= member.file_size, "ZIP member exceeds declared size")
                            staged.write(chunk)
                    require(written == member.file_size, "ZIP member size mismatch")
                    staged.close()
                    if target.exists():
                        require(file_digest(target) == file_digest(staged_path), "ZIP would overwrite different content")
                    else:
                        # Exclusive creation also prevents following a late symlink.
                        with target.open("xb") as output, staged_path.open("rb") as source:
                            shutil.copyfileobj(source, output, MIB)
                        target.chmod(0o755 if (member.external_attr >> 16) & 0o111 else 0o644)
                finally:
                    staged_path.unlink(missing_ok=True)
    return total


def download_archive(repo, artifact, archive):
    expected_size = artifact["size_in_bytes"]
    require(shutil.disk_usage(archive.parent).free >= expected_size + HEADROOM, "Insufficient download disk headroom")
    command = ["gh", "api", f"repos/{repo}/actions/artifacts/{artifact['id']}/zip"]
    with archive.open("xb") as output:
        process = subprocess.Popen(command, stdout=subprocess.PIPE)
        try:
            downloaded = 0
            while chunk := process.stdout.read(MIB):
                downloaded += len(chunk)
                require(downloaded <= expected_size, "Downloaded ZIP exceeds recorded size")
                output.write(chunk)
            require(process.wait() == 0, "Authenticated artifact download failed")
            require(downloaded == expected_size, "Downloaded ZIP size mismatch")
        finally:
            process.stdout.close()
            if process.poll() is None:
                process.terminate()
                process.wait()
    require("sha256:" + file_digest(archive) == artifact["digest"], "Downloaded ZIP digest mismatch")


def download(kind, destination):
    repo, current_sha = context()
    raw = os.environ.get("RELEASE_RECOVERY_MANIFEST", "")
    require(0 < len(raw.encode("utf-8")) < MAX_MANIFEST_BYTES, "Missing or oversized recovery manifest")
    manifest = json.loads(raw)
    require(isinstance(manifest, dict) and manifest.get("schema_version") == 1, "Unsupported recovery manifest")
    require(manifest.get("packaging") == {"repository": repo, "source_sha": current_sha},
            "Manifest belongs to a different packaging repository/source")
    version = version_tag(manifest.get("version"))
    original = manifest.get("producer")
    require(isinstance(original, dict), "Missing original producer")
    producer, artifacts, jobs = inventory(original.get("run_id"), version, repo)
    require(producer == original, "Producer run/source/attempt changed since prepare")
    require(artifacts == manifest.get("artifacts") and jobs == manifest.get("jobs"),
            "Retained artifact metadata or producer jobs changed since prepare")
    destination = safe_directory(destination)
    chosen = [artifact for artifact in artifacts if artifact["kind"] == kind]
    require(chosen, "No artifacts for requested kind")
    expanded = 0
    with tempfile.TemporaryDirectory(prefix=".release-recovery-", dir=destination) as staging:
        for artifact in chosen:
            archive = Path(staging) / f"{artifact['id']}.zip"
            try:
                download_archive(repo, artifact, archive)
                expanded += extract_zip(archive, destination, kind, MAX_TOTAL_EXPANDED - expanded)
            finally:
                archive.unlink(missing_ok=True)
    print(f"Recovered {len(chosen)} {kind} artifacts by immutable ID from run {producer['run_id']}, "
          f"attempt {producer['run_attempt']}; verified ZIP SHA256 digests. Original artifacts retained.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    preparation = commands.add_parser("prepare")
    preparation.add_argument("--run-id", type=int, required=True)
    preparation.add_argument("--version", required=True)
    retrieval = commands.add_parser("download")
    retrieval.add_argument("--kind", choices=tuple(LIMITS), required=True)
    retrieval.add_argument("--destination", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(args.run_id, args.version)
        else:
            download(args.kind, args.destination)
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError, zipfile.BadZipFile) as error:
        # gh errors remain errors; don't print captured output or credentials.
        print(f"Release recovery refused: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
