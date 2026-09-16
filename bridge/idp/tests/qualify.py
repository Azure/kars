#!/usr/bin/env python3
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run only on a hosted native Linux Docker worker; no registry/cluster writes."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import tarfile
import tempfile
import uuid

from contracts import BASE, RPM_MANIFEST, check_modules, check_rootfs, check_scan, require


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout


def export(image, directory, prefix):
    container = run("docker", "create", "--entrypoint", "/usr/local/bin/dex", image).strip()
    path = directory / (prefix + ".tar")
    try:
        run("docker", "export", "--output", str(path), container)
        entries = {}
        documents = {}
        with tarfile.open(path) as archive:
            for member in archive:
                name = member.name.removeprefix("./")
                if member.isfile():
                    with archive.extractfile(member) as stream:
                        data = stream.read()
                    entries[name] = ("file", member.mode, hashlib.sha256(data).hexdigest())
                    if name == RPM_MANIFEST or name.startswith("usr/share/doc/dex/elf-"):
                        documents[name] = data.decode()
                elif member.issym() or member.islnk():
                    entries[name] = ("link", member.mode, member.linkname)
        return entries, documents
    finally:
        run("docker", "rm", container)
        path.unlink(missing_ok=True)


def oidc(image, tools, storage, evidence):
    suffix = uuid.uuid4().hex
    network, config, data, name = (f"dex-test-{kind}-{suffix}" for kind in ("net", "config", "data", "server"))
    created = []
    secret = "DEX_TEST_CLIENT_SECRET=" + secrets.token_urlsafe(32)
    issuer = "http://dex:5556/dex"
    try:
        run("docker", "network", "create", "--internal", network)
        created.append(("network", network))
        for volume in (config, data):
            run("docker", "volume", "create", volume)
            created.append(("volume", volume))
        probe = ["docker", "run", "--rm", "--network", network, "--env", secret,
                 "--mount", f"type=volume,src={config},dst=/config",
                 "--mount", f"type=volume,src={data},dst=/var/dex", tools]
        run(*probe, "config", issuer, "/config", storage)
        # This is the chart's direct invocation; no template-expanding wrapper.
        run("docker", "create", "--name", name, "--network", network,
            "--network-alias", "dex", "--read-only", "--cap-drop", "ALL",
            "--security-opt", "no-new-privileges", "--user", "1001:1001",
            "--tmpfs", "/tmp:rw,noexec,nosuid,size=16m", "--env", secret,
            "--mount", f"type=volume,src={config},dst=/etc/dex,readonly",
            "--mount", f"type=volume,src={data},dst=/var/dex",
            "--entrypoint", "/usr/local/bin/dex", image, "serve", "/etc/dex/config.yaml")
        created.append(("container", name))
        run("docker", "start", name)
        output = run(*probe, "check", issuer, "/config")
        if storage == "sqlite3":
            run("docker", "restart", name)
            output += run(*probe, "resume", issuer, "/config")
        (evidence / f"{storage}-oidc.txt").write_text(output)
    finally:
        for kind, identifier in reversed(created):
            if kind == "container":
                run("docker", "rm", "--force", identifier)
            else:
                run("docker", kind, "rm", identifier)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--tools-image", required=True)
    parser.add_argument("--evidence", required=True, type=Path)
    parser.add_argument("--trivy", default="trivy")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    check_modules(root)
    args.evidence.mkdir(parents=True, exist_ok=False)
    image = json.loads(run("docker", "image", "inspect", args.image))[0]
    tools = json.loads(run("docker", "image", "inspect", args.tools_image))[0]
    daemon_os, daemon_arch = run("docker", "info", "--format", "{{.OSType}}/{{.Architecture}}").strip().split("/")
    daemon_arch = {"x86_64": "amd64", "aarch64": "arm64"}.get(daemon_arch, daemon_arch)
    require(daemon_os == "linux" and daemon_arch == image["Architecture"], "native Linux worker required")
    require(image["Os"] == "linux" and image["Architecture"] == tools["Architecture"],
            "runtime/tools architecture mismatch")
    # Resolve mutable local tags once. All subsequent checks address image IDs.
    image_id, tools_id = image["Id"], tools["Id"]
    (args.evidence / "image.json").write_text(json.dumps(image, indent=2))
    require(image["Config"]["User"] == "1001:1001", "unexpected runtime user")
    require(image["Config"]["Entrypoint"] == ["/usr/local/bin/dex"], "unexpected entrypoint")
    require(image["Config"]["Cmd"] == ["serve", "/etc/dex/config.yaml"], "unexpected command")
    run("docker", "pull", "--platform", "linux/" + image["Architecture"], BASE)
    with tempfile.TemporaryDirectory(prefix="dex-qualification-") as temporary:
        scratch = Path(temporary)
        base, _ = export(BASE, scratch, "base")
        runtime, documents = export(image_id, scratch, "runtime")
        check_rootfs(base, runtime, documents[RPM_MANIFEST])
        (args.evidence / "runtime-inventory.json").write_text(json.dumps(runtime, indent=2))
        for name, contents in documents.items():
            (args.evidence / Path(name).name).write_text(contents)
        headers = documents["usr/share/doc/dex/elf-program-headers.txt"]
        match = re.search(r"Requesting program interpreter: ([^\]]+)", headers)
        require(match is not None, "CGO Dex must have a real runtime ELF interpreter")
        loader = match.group(1)
        require(loader.lstrip("/") in base, "ELF interpreter is not provided by pinned runtime")
        linked = run("docker", "run", "--rm", "--network", "none", "--read-only",
                     "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                     "--entrypoint", loader, image_id, "--list", "/usr/local/bin/dex")
        require("not found" not in linked and "libc.so.6" in linked, "native library closure failed")
        (args.evidence / "runtime-linkage.txt").write_text(linked)
        for storage in ("memory", "sqlite3"):
            oidc(image_id, tools_id, storage, args.evidence)
        environment = {k: v for k, v in os.environ.items() if not k.startswith("TRIVY_")}
        version = run(args.trivy, "--version", env=environment)
        require(re.search(r"Version: 0\.70\.0\b", version), "qualification requires Trivy 0.70.0")
        empty_ignore = scratch / "empty.trivyignore"
        empty_config = scratch / "empty.yaml"
        empty_ignore.write_text("")
        empty_config.write_text("{}\n")
        cache = scratch / "trivy-cache"
        common = [args.trivy, "--config", str(empty_config), "--cache-dir", str(cache), "image"]
        run(*common, "--download-db-only", env=environment)
        scan_path = args.evidence / "trivy-high-critical.json"
        scan = subprocess.run(
            [*common, "--image-src", "docker", "--scanners", "vuln",
             "--ignorefile", str(empty_ignore), "--severity", "HIGH,CRITICAL",
             "--list-all-pkgs", "--skip-db-update", "--exit-code", "1",
             "--format", "json", "--output", str(scan_path), image_id],
            text=True, capture_output=True, env=environment,
        )
        (args.evidence / "trivy-stderr.txt").write_text(scan.stderr)
        require(scan.returncode == 0, "final Trivy scan failed; see trivy-high-critical.json/stderr")
        check_scan(json.loads(scan_path.read_text()))
        (args.evidence / "trivy-version.txt").write_text(run(args.trivy, "--cache-dir", str(cache),
                                                           "--version", env=environment))
    (args.evidence / "runtime-passed.json").write_text(json.dumps({
        "image": image_id, "tools": tools_id, "base": BASE,
        "checks": ["base-inventory-and-trust", "native-linkage", "memory-oidc",
                   "sqlite-oidc-and-refresh-after-restart", "latest-db-high-critical"],
        "notCovered": ["upstream-service-integration-tests", "external-connector-enrollment",
                       "independent-rebuild-comparison", "deployment"],
    }, indent=2))
    print("Runtime gates passed. Upstream suites, rebuild comparison and configured connector tests remain separate gates.")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        # Do not echo Docker command arguments containing ephemeral credentials.
        raise SystemExit(f"Hosted command failed ({error.returncode}): {error.stderr or error.stdout}") from None
    except (ValueError, OSError, KeyError) as error:
        raise SystemExit(str(error)) from None
