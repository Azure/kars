# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fail-closed native-contract scope selection from the actual Git tree diff."""

import argparse
from pathlib import PurePosixPath
import re
import subprocess


ROOT_DOCUMENTS = {"README.md", "CHANGELOG.md", "CONTRIBUTING.md", "LICENSE", "NOTICE"}
DOCUMENT_SUFFIXES = {".md", ".txt", ".png", ".svg", ".jpg", ".jpeg", ".gif", ".mmd"}


def native_required(paths, core_only=False):
    for path in paths:
        if core_only and path.startswith("bridge/"):
            continue
        if path in ROOT_DOCUMENTS:
            continue
        if path.startswith("docs/") and PurePosixPath(path).suffix in DOCUMENT_SUFFIXES:
            continue
        # New/shared packages, shipped skills, and unknown paths require proof.
        return True
    return False


def classify(event, base, head, core_only=False):
    if not event or event == "pull_request_target":
        raise ValueError("Unsupported native contract event")
    # Reusable CI retains its caller's event, including release/schedule.
    # Every non-PR event must keep the existing full-qualification behavior.
    if event != "pull_request":
        return True
    if not all(re.fullmatch(r"[a-f0-9]{40}", value) for value in (base, head)):
        raise ValueError("Native contract scope requires exact base and head revisions")
    result = subprocess.run(
        ["git", "diff", "--no-renames", "--name-only", "-z", base, head, "--"],
        check=True, capture_output=True, timeout=30,
    )
    # Keep both sides of moves: moving a runtime file into docs is still code.
    return native_required(
        (path.decode("utf-8") for path in result.stdout.split(b"\0") if path), core_only,
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--event", required=True)
    parser.add_argument("--base", default="")
    parser.add_argument("--head", default="")
    parser.add_argument("--output", choices=("required", "code", "run"), default="required")
    parser.add_argument("--core-only", action="store_true")
    args = parser.parse_args()
    print(args.output + "=" + str(classify(args.event, args.base, args.head, args.core_only)).lower())
