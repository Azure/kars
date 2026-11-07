#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Prepare and verify a hosted-only Dex resolver candidate, never promote it."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
CHECKOUT = ROOT.parents[1]
sys.path.insert(0, str(ROOT / "tests"))
from contracts import BASELINE_SNAPSHOTS, LOCK_FILES, check_modules, require

FIXED_CRYPTO = "golang.org/x/crypto@v0.58.1-0.20261009181718-2f1f834330b2"
WORKFLOW = ".github/workflows/bridge-idp-locks.yml"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def source_files():
    files = {name: ROOT / name for name in
             (".dockerignore", "Dockerfile", "Dockerfile.locks", "LICENSE", "NOTICE", "README.md")}
    for directory in ("locks", "patches", "scripts", "tests"):
        for path in (ROOT / directory).rglob("*"):
            if path.is_file() and "__pycache__" not in path.parts and path.name != ".DS_Store":
                require(not path.is_symlink(), "resolver source cannot contain symlinks")
                files[path.relative_to(ROOT).as_posix()] = path
    return files


def candidate_requests():
    lines = (ROOT / "locks/requests.txt").read_text().splitlines()
    crypto = [line for line in lines if line.startswith("golang.org/x/crypto@")]
    require(len(crypto) == 1 and crypto[0] in ("golang.org/x/crypto@v0.58.0", FIXED_CRYPTO),
            "unexpected reviewed crypto baseline; review the maintenance candidate")
    return ("\n".join(FIXED_CRYPTO if line == crypto[0] else line for line in lines) + "\n").encode()


def reviewed_inputs():
    files = {"bridge/idp/" + name: digest(path.read_bytes()) for name, path in source_files().items()}
    files[WORKFLOW] = digest((CHECKOUT / WORKFLOW).read_bytes())
    return {"schema": 1, "crypto_request": FIXED_CRYPTO,
            "requests_sha256": digest(candidate_requests()), "source_sha256": dict(sorted(files.items()))}


def prepare(context, evidence):
    check_modules(ROOT)
    require(not context.exists() and not evidence.exists(), "candidate/evidence output already exists")
    inputs = reviewed_inputs()
    context.mkdir(parents=True)
    evidence.mkdir(parents=True)
    provenance = {"inputs": inputs, "github": {key: os.environ.get(key, "") for key in
                  ("GITHUB_REPOSITORY", "GITHUB_SHA", "SOURCE_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "GITHUB_EVENT_NAME")}}
    (evidence / "provenance.json").write_text(json.dumps(provenance, indent=2, sort_keys=True) + "\n")
    for name, path in source_files().items():
        destination = context / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, destination)
    # The committed requests/generated pair remains coherent for ordinary image builds.
    (context / "locks/requests.txt").write_bytes(candidate_requests())


def verify(evidence, seal=False):
    provenance_bytes = (evidence / "provenance.json").read_bytes()
    provenance = json.loads(provenance_bytes)
    require(provenance["inputs"] == reviewed_inputs(), "hosted resolver source/version input drift")
    identity = provenance["github"]
    require(re.fullmatch(r"[0-9a-f]{40}", identity["SOURCE_SHA"]) and
            re.fullmatch(r"[0-9a-f]{40}", identity["GITHUB_SHA"]) and
            re.fullmatch(r"[\w.-]+/[\w.-]+", identity["GITHUB_REPOSITORY"]) and
            identity["GITHUB_RUN_ID"].isdigit() and identity["GITHUB_RUN_ATTEMPT"].isdigit() and
            identity["GITHUB_EVENT_NAME"] in ("pull_request", "workflow_dispatch"),
            "missing or invalid GitHub resolver provenance")
    if not seal:
        require((evidence / "job-status.txt").read_text().strip() == "success" and
                (evidence / "source-diff.log").read_bytes() == b"",
                "failed hosted attempts are evidence, not importable locks")
    resolved = evidence / "resolved"
    require(resolved.is_dir() and not resolved.is_symlink(), "missing or unsafe hosted resolver output")
    entries = list(resolved.rglob("*"))
    require(not any(path.is_symlink() for path in entries), "hosted output cannot contain symlinks")
    require({path.relative_to(resolved).as_posix() for path in entries if path.is_file()} == LOCK_FILES,
            "unexpected or missing hosted resolver files")
    require(sum(path.stat().st_size for path in entries if path.is_file()) <= 16 * 1024 * 1024,
            "hosted resolver output exceeds bounded lock size")
    # The existing validator checks both real resolver inventories, exact MVS selections,
    # source inputs, toolchain, replacements and complete per-file SHA-256 coverage.
    with tempfile.TemporaryDirectory(prefix=".dex-hosted-verify-", dir=evidence.parent) as directory:
        stage = Path(directory)
        (stage / "locks").mkdir()
        shutil.copytree(resolved, stage / "locks/generated")
        shutil.copyfile(ROOT / "locks/inputs.lock", stage / "locks/inputs.lock")
        (stage / "locks/requests.txt").write_bytes(candidate_requests())
        check_modules(stage)
    for name in BASELINE_SNAPSHOTS:
        require((resolved / name).read_bytes() == (ROOT / "locks/generated" / name).read_bytes(),
                f"hosted candidate changed the upstream baseline: {name}")
    result = {"provenance_sha256": digest(provenance_bytes),
              "locks_sha256": digest((resolved / "SHA256SUMS").read_bytes())}
    if seal:
        require(not (evidence / "verified.json").exists(), "hosted verification already recorded")
        (evidence / "verified.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    else:
        require(json.loads((evidence / "verified.json").read_text()) == result,
                "hosted verification digest mismatch")
    return {name: (resolved / name).read_bytes() for name in LOCK_FILES}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("prepare", "seal", "verify"))
    parser.add_argument("--context", type=Path)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    if args.operation == "prepare":
        require(args.context is not None, "prepare requires --context")
        prepare(args.context, args.evidence)
    else:
        verify(args.evidence, seal=args.operation == "seal")
    print(f"Hosted lock candidate {args.operation} passed; no reviewed locks were promoted.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError) as error:
        raise SystemExit(str(error)) from None
