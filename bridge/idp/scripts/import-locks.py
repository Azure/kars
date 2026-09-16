#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Import active Go locks and archival .snapshot baselines from a no-push build log."""

import base64
import gzip
import hashlib
import io
from pathlib import Path
import re
import shutil
import sys
import tarfile
import tempfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))
from contracts import LOCK_FILES, check_modules, require

MAX_ARCHIVE_BYTES = 16 * 1024 * 1024


def decode_artifact(log):
    blocks = re.findall(
        r"(?m)^KARS_DEX_LOCKS_BASE64_BEGIN\r?\n([A-Za-z0-9+/=]+)\r?\nKARS_DEX_LOCKS_BASE64_END\r?$",
        log,
    )
    require(len(blocks) == 1, "expected exactly one unprefixed lock artifact in the raw ACR log")
    require(len(blocks[0]) < 8 * 1024 * 1024, "unexpectedly large lock artifact")
    payload = base64.b64decode(blocks[0], validate=True)
    digests = re.findall(r"(?m)^([0-9a-f]{64})  /tmp/dex-locks\.tar\.gz\r?$", log)
    require(len(digests) == 1 and hashlib.sha256(payload).hexdigest() == digests[0],
            "missing or mismatched transport SHA-256")
    # Bound the entire tar, including headers and directory entries, before
    # tarfile can allocate an unbounded member index for a compressed archive.
    with gzip.GzipFile(fileobj=io.BytesIO(payload)) as compressed:
        expanded = compressed.read(MAX_ARCHIVE_BYTES + 1)
    require(len(expanded) <= MAX_ARCHIVE_BYTES, "expanded artifact exceeds bounded lock size")
    result = {}
    size = 0
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as archive:
        for member in archive:
            path = Path(member.name)
            require(not path.is_absolute() and ".." not in path.parts, "unsafe archive path")
            if member.isdir():
                continue
            name = str(path)
            require(member.isfile() and name in LOCK_FILES and name not in result,
                    f"unexpected or duplicate lock member: {name}")
            size += member.size
            require(size <= MAX_ARCHIVE_BYTES, "expanded artifact exceeds bounded lock size")
            with archive.extractfile(member) as stream:
                result[name] = stream.read()
    require(set(result) == LOCK_FILES, "incomplete Go resolver artifact")
    return result


def main():
    require(len(sys.argv) == 2, "usage: import-locks.py raw-acr-build.log")
    target = ROOT / "locks/generated"
    require(not target.exists(), "generated locks already exist; review them rather than overwriting")
    files = decode_artifact(Path(sys.argv[1]).read_text())
    with tempfile.TemporaryDirectory(prefix=".dex-lock-import-", dir=ROOT / "locks") as temporary:
        stage = Path(temporary)
        generated = stage / "locks/generated"
        generated.mkdir(parents=True)
        for name, data in files.items():
            path = generated / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        for name in ("inputs.lock", "requests.txt"):
            shutil.copyfile(ROOT / "locks" / name, stage / "locks" / name)
        check_modules(stage)
        generated.rename(target)
    print("Imported active Go locks and archival .snapshot baselines. Review dependencies.patch and all selected modules before building.")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, EOFError, tarfile.TarError, zlib.error) as error:
        raise SystemExit(str(error)) from None
