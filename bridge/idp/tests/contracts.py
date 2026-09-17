# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import hashlib
import json
from pathlib import Path
import re

BASE = (
    "mcr.microsoft.com/azurelinux/distroless/base:3.0@sha256:"
    "4377af4aa7a810b7d59f691eae5066895a71aa3eee4cfb4eba527bbebff16479"
)
RPM_MANIFEST = "var/lib/rpmmanifest/container-manifest-2"
ACTIVE_LOCK_FILES = {"go.mod", "go.sum", "api/v2/go.mod", "api/v2/go.sum"}
BASELINE_SNAPSHOTS = {f"upstream/{name}.snapshot" for name in ACTIVE_LOCK_FILES}
LOCK_FILES = ACTIVE_LOCK_FILES | BASELINE_SNAPSHOTS | {
    "modules.json", "api-modules.json", "graph.txt", "toolchain.txt",
    "inputs.lock", "requests.txt", "dependencies.patch", "SHA256SUMS",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def json_stream(text):
    decoder = json.JSONDecoder()
    while text.strip():
        text = text.lstrip()
        value, end = decoder.raw_decode(text)
        yield value
        text = text[end:]


def module_inventory(text):
    modules = {}
    for module in json_stream(text):
        require(isinstance(module, dict) and isinstance(module.get("Path"), str),
                "invalid module inventory record")
        require(not module.get("Error"), f"module resolver error: {module['Path']}")
        require(module["Path"] not in modules, f"duplicate module inventory: {module['Path']}")
        modules[module["Path"]] = module
    return modules


def check_modules(root):
    """Validate the reviewed resolver artifact, without inventing Go checksums."""
    root = Path(root)
    generated = root / "locks/generated"
    require(generated.is_dir(), "hosted generated locks are missing; runtime build is blocked")
    require(not generated.is_symlink(), "lock artifact cannot be a symlink")
    entries = list(generated.rglob("*"))
    require(not any(path.is_symlink() for path in entries), "lock artifact cannot contain symlinks")
    actual = {str(path.relative_to(generated)) for path in entries if path.is_file()}
    require(actual == LOCK_FILES, "unexpected or missing lock artifact files")
    for name in ("inputs.lock", "requests.txt"):
        require((generated / name).read_bytes() == (root / "locks" / name).read_bytes(),
                f"resolver input drift: {name}")
    records = {}
    for line in (generated / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        require(re.fullmatch(r"[0-9a-f]{64}", digest), "invalid artifact SHA-256")
        path = Path(name)
        require(not path.is_absolute() and ".." not in path.parts, "unsafe checksum path")
        require(name not in records, "duplicate checksum path")
        records[name] = digest
        require(hashlib.sha256((generated / path).read_bytes()).hexdigest() == digest,
                f"artifact digest mismatch: {name}")
    require({"./" + name for name in actual - {"SHA256SUMS"}} == set(records),
            "artifact checksum coverage is incomplete")
    require(re.fullmatch(r"go version go1\.26\.8 linux/(amd64|arm64)\n",
                         (generated / "toolchain.txt").read_text()),
            "unexpected resolver toolchain")
    inventories = {name: module_inventory((generated / name).read_text())
                   for name in ("modules.json", "api-modules.json")}
    selected = inventories["modules.json"]
    for line in (root / "locks/requests.txt").read_text().splitlines():
        name, version = line.split("@")
        require(selected.get(name, {}).get("Version") == version, f"unreviewed selection: {name}")
        require("Replace" not in selected[name], f"unexpected replacement: {name}")
    for name, modules in inventories.items():
        expected_main = "github.com/dexidp/dex" + ("/api/v2" if name == "api-modules.json" else "")
        require([m["Path"] for m in modules.values() if m.get("Main")] == [expected_main],
                f"unexpected main module in {name}")
        for module in modules.values():
            if "Replace" in module:
                require(name == "modules.json" and module["Path"] == "github.com/dexidp/dex/api/v2" and
                        module["Replace"]["Path"] == "./api/v2",
                        f"unexpected module replacement in {name}")
    require(selected.get("github.com/dexidp/dex/api/v2", {}).get("Replace", {}).get("Path") == "./api/v2",
            "upstream local API replacement is missing")
    api = inventories["api-modules.json"]
    require(api.get("google.golang.org/grpc", {}).get("Version") == "v1.83.2", "nested API gRPC is unpatched")
    for name in ("go.mod", "api/v2/go.mod"):
        manifest = (generated / name).read_text()
        require("\ngo 1.26.0\n" in manifest, f"unexpected Go requirement: {name}")
        # Go may omit a redundant toolchain directive; the pinned builder and
        # GOTOOLCHAIN=local remain authoritative in that case.
        directives = [line for line in manifest.splitlines() if line.startswith("toolchain ")]
        require(not directives or directives == ["toolchain go1.26.8"],
                f"unexpected automatic toolchain: {name}")


def check_rootfs(base, runtime, manifest):
    # Docker injects these three per-container files during export.
    injected = {"etc/hosts", "etc/hostname", "etc/resolv.conf"}
    for name, record in base.items():
        if name not in injected:
            require(runtime.get(name) == record, f"base file modified/removed: {name}")
    require(len(manifest.strip().splitlines()) == 14, "expected 14 Azure Linux RPM inventory records")
    require(RPM_MANIFEST in runtime, "RPM inventory missing")
    require(any("ca-trust" in name or name.endswith("ca-certificates.crt") for name in base),
            "base CA trust inventory missing")
    for name in runtime.keys() - base.keys() - injected:
        require(name == "usr/local/bin/dex" or
                name.startswith(("srv/dex/web/", "usr/share/doc/dex/")),
                f"unexpected runtime payload: {name}")
    for name in ("usr/local/bin/docker-entrypoint", "usr/local/bin/gomplate",
                 "bin/sh", "bin/bash", "usr/bin/sh", "usr/bin/bash",
                 "usr/bin/tdnf", "usr/bin/npm", "usr/local/go/bin/go", "usr/bin/gcc"):
        require(name not in runtime, f"shipping tool/unused entrypoint: {name}")

def resolve_base_file(inventory, requested):
    require(isinstance(requested, str) and requested.startswith("/")
            and len(requested) <= 4096 and "\0" not in requested, "invalid ELF interpreter path")
    pending = requested.split("/")
    resolved = []
    links = 0
    while pending:
        part = pending.pop(0)
        if part in ("", "."):
            continue
        if part == "..":
            require(resolved, "base link escapes image root")
            resolved.pop()
            continue
        name = "/".join([*resolved, part])
        entry = inventory.get(name)
        if entry is not None and entry[0] in ("symlink", "hardlink"):
            links += 1
            require(links <= 32, "base link resolution exceeds its bound")
            target = entry[2]
            require(isinstance(target, str) and target and len(target) <= 4096
                    and "\0" not in target, "invalid base link target")
            if entry[0] == "hardlink" or target.startswith("/"):
                resolved = []
            pending = target.split("/") + pending
        else:
            resolved.append(part)
    name = "/".join(resolved)
    require(inventory.get(name, (None,))[0] == "file", "ELF interpreter is not provided by pinned runtime")
    return name


def check_scan(report):
    require(report.get("Metadata", {}).get("OS", {}).get("Family") == "azurelinux",
            "scanner failed to identify Azure Linux")
    results = report.get("Results", [])
    require(any(r.get("Type") == "gobinary" and r["Target"].lstrip("/") == "usr/local/bin/dex"
                for r in results), "scanner failed to inventory the Dex Go binary")
    for result in results:
        require(not result.get("Vulnerabilities"), "HIGH/CRITICAL scan findings remain")
    os_results = [r for r in results if r.get("Class") == "os-pkgs"]
    packages = {p["Name"] for r in os_results for p in r.get("Packages", [])}
    require(len(packages) == 14, "scanner did not inventory all 14 base RPM packages")
