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


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: headlamp_build_evidence.py EVIDENCE_DIRECTORY")
    verify(Path(sys.argv[1]))
