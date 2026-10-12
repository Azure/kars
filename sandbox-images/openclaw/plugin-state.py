#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Build genuine installer state; initialize it offline without replacing live data."""
from contextlib import closing
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile

MANAGED = Path("/opt/openclaw-managed")
SEED = Path("/opt/openclaw-plugin-seed.sqlite")


def backup(source, destination):
    with closing(sqlite3.connect(source.resolve().as_uri() + "?mode=ro", uri=True)) as src:
        with closing(sqlite3.connect(destination)) as dst:
            src.backup(dst)
            if dst.execute("PRAGMA journal_mode=DELETE").fetchone() != ("delete",):
                raise RuntimeError("Plugin seed must not require writable WAL sidecars")
            if dst.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
                raise RuntimeError("Plugin installer database failed integrity check")


def build(manifest_path):
    manifest = json.loads(manifest_path.read_text())
    version = manifest["openclawVersion"]
    installed = json.loads(Path("/usr/local/lib/node_modules/openclaw/package.json").read_text())
    if installed["version"] != version:
        raise RuntimeError("Plugin versions must match the installed OpenClaw version")
    env = dict(os.environ, OPENCLAW_STATE_DIR=str(MANAGED),
               OPENCLAW_CONFIG_PATH=str(MANAGED / "openclaw.json"),
               OPENCLAW_NO_RESPAWN="1", NO_COLOR="1")
    registry = subprocess.check_output(["npm", "config", "get", "registry"], text=True).strip()
    if registry != "https://registry.npmjs.org/":
        raise RuntimeError("External plugins require the official npm registry")
    for plugin in manifest["plugins"]:
        spec = plugin["package"] + "@" + version
        integrity = json.loads(subprocess.check_output(
            ["npm", "view", spec, "dist.integrity", "--json"], text=True, timeout=60))
        if integrity != plugin["integrity"]:
            raise RuntimeError("Registry integrity changed for " + spec)
        subprocess.run(
            ["openclaw", "plugins", "install", spec, "--pin", "--no-enable",
             "--accept-capabilities", "--force"], env=env, check=True, timeout=600)
        matches = list((MANAGED / "npm/projects").glob(
            "*/node_modules/" + plugin["package"] + "/openclaw.plugin.json"))
        if len(matches) != 1:
            raise RuntimeError("Expected one installed package for " + spec)
        actual = json.loads(matches[0].read_text())
        package = json.loads((matches[0].parent / "package.json").read_text())
        if actual["id"] != plugin["id"] or package["version"] != version:
            raise RuntimeError("Installed plugin identity mismatch for " + spec)
    if SEED.exists():
        raise RuntimeError("Refusing to replace an existing build seed")
    backup(MANAGED / "state/openclaw.sqlite", SEED)
    print("[kars] Built immutable plugin seed " + hashlib.sha256(SEED.read_bytes()).hexdigest())


def initialize(seed, root):
    digest = hashlib.sha256(seed.read_bytes()).hexdigest()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    if root.is_symlink():
        raise RuntimeError("Plugin state root must not be a symlink")
    with os.fdopen(os.open(root / ".kars-plugin-seed.lock",
                           os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600), "r+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        directory = root / "state"
        if directory.is_symlink():
            raise RuntimeError("Plugin database directory must not be a symlink")
        directory.mkdir(exist_ok=True, mode=0o700)
        database = directory / "openclaw.sqlite"
        marker = directory / "kars-plugin-seed.sha256"
        if database.is_symlink() or marker.is_symlink():
            raise RuntimeError("Plugin database and seed marker must not be symlinks")
        if database.exists():
            if not marker.is_file() or marker.read_text().strip() != digest:
                raise RuntimeError(
                    "Existing OpenClaw database has a different or missing image seed binding; "
                    "recreate the sandbox pod (ephemeral state). Existing data was not replaced.")
            print("[kars] Retained existing OpenClaw state for the current image seed")
            return
        if marker.exists() and marker.read_text().strip() != digest:
            raise RuntimeError("Incomplete state initialization belongs to another image seed")
        fd, name = tempfile.mkstemp(prefix=".kars-plugin-seed-", dir=directory)
        os.close(fd)
        temporary = Path(name)
        try:
            backup(seed, temporary)
            with temporary.open("rb") as source:
                os.fsync(source.fileno())
            # Publish the binding first: an interrupted initialization has no live
            # database to overwrite, and can safely repeat this exact seed backup.
            with marker.open("w") as out:
                out.write(digest + "\n")
                out.flush()
                os.fsync(out.fileno())
            os.replace(temporary, database)
        finally:
            temporary.unlink(missing_ok=True)
        print("[kars] Initialized offline OpenClaw plugin state from the image seed")


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "build":
        build(Path(sys.argv[2]))
    elif len(sys.argv) == 3 and sys.argv[1] == "initialize":
        initialize(SEED, Path(sys.argv[2]))
    else:
        raise SystemExit("Usage: plugin-state.py build MANIFEST | initialize STATE_DIRECTORY")
