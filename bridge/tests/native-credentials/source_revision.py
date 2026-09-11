"""Bind Bridge/core qualification to the same checked-out monorepo commit."""

import os
from pathlib import Path
import re
import subprocess


def checked_revision():
    repository = Path(__file__).resolve().parents[3]
    result = subprocess.run(
        ["git", "-C", str(repository), "rev-parse", "HEAD"],
        capture_output=True, text=True, timeout=10, check=False,
    )
    revision = result.stdout.strip()
    if result.returncode or re.fullmatch(r"[a-f0-9]{40}", revision) is None:
        raise RuntimeError("Monorepo source revision is unavailable")
    expected = os.environ.get("CORE_REVISION")
    if expected is not None and expected != revision:
        raise RuntimeError("Qualification source differs from the workflow's exact commit")
    return revision


CORE_REVISION = checked_revision()
