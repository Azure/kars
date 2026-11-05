# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Verify the exact generated inputs exported by the shipping Headlamp image."""

import hashlib
import json
from pathlib import Path
import re
import sys


INPUTS = {"package.json", "package-lock.json", "vite.config.ts"}


def verify(root: Path) -> None:
    evidence = json.loads((root / "frontend-evidence.json").read_text())
    hashes = evidence.get("buildInputs")
    if not isinstance(hashes, dict) or set(hashes) != INPUTS:
        raise ValueError("Headlamp evidence must bind all three canonical build inputs")
    inputs = root / "frontend-build"
    if inputs.is_symlink() or not inputs.is_dir():
        raise ValueError("Headlamp build inputs must be an extracted directory")
    for name, expected in hashes.items():
        if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
            raise ValueError(f"Invalid Headlamp build-input digest: {name}")
        path = inputs / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"Missing regular Headlamp build input: {name}")
        if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            raise ValueError(f"Headlamp build-input digest mismatch: {name}")


def verify_npm_inventory(report: Path) -> None:
    scan = json.loads(report.read_text())
    results = scan.get("Results") if isinstance(scan, dict) else None
    if isinstance(results, list):
        for result in results:
            if not isinstance(result, dict) or result.get("Type") != "npm":
                continue
            target = result.get("Target")
            if not isinstance(target, str) or Path(target).name != "package-lock.json":
                continue
            packages = result.get("Packages")
            if isinstance(packages, list) and packages and all(
                isinstance(package, dict)
                and all(isinstance(package.get(key), str) and package[key] for key in ("Name", "Version"))
                for package in packages
            ):
                return
    raise ValueError("Headlamp filesystem scan must contain a nonempty npm lock inventory")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--npm-inventory":
        verify_npm_inventory(Path(sys.argv[2]))
    elif len(sys.argv) == 2:
        verify(Path(sys.argv[1]))
    else:
        raise SystemExit("usage: headlamp_build_evidence.py EVIDENCE_DIRECTORY | --npm-inventory REPORT")
