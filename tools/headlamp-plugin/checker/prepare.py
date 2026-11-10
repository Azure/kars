#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Reproduce the attributed checker archive without installing or compiling it."""

import argparse
import base64
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parent
OUTPUT = ROOT / "fork-ts-checker-webpack-plugin-8.0.0-kars.tgz"


def prepare(upstream_bytes):
    provenance = json.loads((ROOT / "upstream.json").read_text())
    digest = "sha512-" + base64.b64encode(hashlib.sha512(upstream_bytes).digest()).decode()
    if digest != provenance["integrity"]:
        raise ValueError("Upstream checker integrity mismatch")
    files = {}
    with tarfile.open(fileobj=io.BytesIO(upstream_bytes), mode="r:gz") as archive:
        for entry in archive.getmembers():
            name = PurePosixPath(entry.name)
            if name.is_absolute() or ".." in name.parts or name.parts[0] != "package":
                raise ValueError("Unexpected upstream archive path")
            if entry.isdir():
                continue
            if not entry.isfile() or entry.name in files:
                raise ValueError("Unexpected upstream archive entry")
            files[entry.name] = (archive.extractfile(entry).read(), entry.mode)
    package_path = "package/package.json"
    package = json.loads(files[package_path][0])
    if package["name"] != provenance["name"] or package["version"] != provenance["version"]:
        raise ValueError("Unexpected upstream package identity")
    if package["dependencies"].pop("deepmerge") != "^4.2.2":
        raise ValueError("Unexpected upstream merge dependency")
    package["dependencies"]["deepmerge-ts"] = provenance["replacement"]["version"]
    package["engines"]["node"] = ">=16.9.0"
    package["karsPatch"] = {
        "upstream": provenance,
        "consumer": "lib/plugin.js",
        "description": "Replace the options merger only; preserve checker workers and Webpack hooks",
        "mergeOptionsSha256": hashlib.sha256((ROOT / "merge-options.cjs").read_bytes()).hexdigest(),
    }
    files[package_path] = ((json.dumps(package, indent=2) + "\n").encode(), files[package_path][1])
    plugin_path = "package/lib/plugin.js"
    plugin = files[plugin_path][0].decode()
    for before, after in [
        ('const deepmerge_1 = __importDefault(require("deepmerge"));',
         'const mergeOptions = require("./merge-options.cjs");'),
        ('this.options = (0, deepmerge_1.default)(externalOptions || {}, options || {});',
         'this.options = mergeOptions(externalOptions || {}, options || {});'),
    ]:
        if plugin.count(before) != 1:
            raise ValueError("Unexpected upstream checker consumer")
        plugin = plugin.replace(before, after)
    files[plugin_path] = (plugin.encode(), files[plugin_path][1])
    files["package/lib/merge-options.cjs"] = ((ROOT / "merge-options.cjs").read_bytes(), 0o644)
    files["package/KARS-PATCH.json"] = ((json.dumps(provenance, indent=2) + "\n").encode(), 0o644)
    files["package/KARS-LICENSE"] = ((ROOT / "LICENSE").read_bytes(), 0o644)
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as archive:
        for name, (content, mode) in sorted(files.items()):
            entry = tarfile.TarInfo(name)
            entry.size, entry.mode, entry.mtime = len(content), mode, 0
            archive.addfile(entry, io.BytesIO(content))
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0, compresslevel=9) as compressed:
        compressed.write(raw.getvalue())
    return output.getvalue()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, help="Existing original archive; its pinned SHA-512 is always verified")
    parser.add_argument("--check", action="store_true", help="Verify the committed archive instead of writing it")
    args = parser.parse_args()
    provenance = json.loads((ROOT / "upstream.json").read_text())
    if args.upstream:
        upstream = args.upstream.read_bytes()
    else:
        with urllib.request.urlopen(provenance["url"], timeout=30) as response:
            upstream = response.read(2 * 1024 * 1024)
    prepared = prepare(upstream)
    if args.check:
        if OUTPUT.read_bytes() != prepared:
            raise ValueError("Packaged checker does not match the reviewed recipe/source")
        print("Checker archive matches pinned upstream and patch recipe")
    else:
        OUTPUT.write_bytes(prepared)
        print(f"Wrote {OUTPUT.name}: sha256:{hashlib.sha256(prepared).hexdigest()}")


if __name__ == "__main__":
    main()
