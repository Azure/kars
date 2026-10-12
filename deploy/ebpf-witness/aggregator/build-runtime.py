# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Build the pinned upstream client and daemon with reviewed locks, never at runtime."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile

COMMIT = "7a314380ff61534ea70df9fb086505f958b2dff9"
SOURCE_SHA256 = "934060feae4ecab32644704c4e272e06b7c0da5a4f67429ba04b0113751946e4"
VERSION = "v0.53.2-kars-security"
DAEMON = "ghcr.io/inspektor-gadget/inspektor-gadget:latest@sha256:39ebe601aff064f531aa0630194d9a7bbdb5060de4f290d7ec2fd678f1dd5c10"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def configuration():
    arch = os.environ["TARGETARCH"]
    revision = os.environ["SOURCE_REVISION"]
    if arch not in ("amd64", "arm64"):
        raise SystemExit("only Linux amd64 and arm64 clients are supported")
    if sys.version_info < (3, 12) or not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise SystemExit("Python 3.12+ and a full public source revision are required")
    bases = {}
    for name, family in [("PYTHON_BASE", "distroless"), ("PYTHON_BUILD_BASE", "base")]:
        image = os.environ[name]
        if not re.fullmatch(
            rf"mcr\.microsoft\.com/azurelinux/{family}/python:[\w.-]+@sha256:[0-9a-f]{{64}}",
            image,
        ):
            raise SystemExit(f"{name} must use a reviewed image digest from Azure Linux {family}/python")
        bases[name.lower()] = image
    return dict(source="https://github.com/Azure/kars", source_revision=revision,
                architecture=arch, **bases)


def package(directory):
    directory = Path(directory)
    root = directory / "runtime-root"
    root.mkdir()
    files = {
        "kubectl-gadget": "usr/local/bin/kubectl-gadget",
        "gadgettracermanager": "usr/bin/gadgettracermanager",
        "build.json": "opt/witness/build.json",
        "witness.py": "opt/witness/witness.py",
        "kubectl-gadget.buildinfo": "opt/witness/provenance/kubectl-gadget.buildinfo",
        "gadgettracermanager.buildinfo": "opt/witness/provenance/gadgettracermanager.buildinfo",
        "client.go.mod": "opt/witness/provenance/client.go.mod",
        "client.go.sum": "opt/witness/provenance/client.go.sum",
        "LICENSE": "usr/share/licenses/inspektor-gadget/LICENSE",
        "THIRD_PARTY_NOTICES.md": "usr/share/licenses/inspektor-gadget/NOTICE",
    }
    for name, destination in files.items():
        target = root / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(directory / name, target)
        target.chmod(0o555 if name in ("kubectl-gadget", "gadgettracermanager") else 0o444)
    for path in [root, *root.rglob("*")]:
        if path.is_dir():
            path.chmod(0o755)


def build(directory):
    metadata = configuration()
    directory = Path(directory).resolve()
    archive = directory / "source.tar.gz"
    if digest(archive) != SOURCE_SHA256:
        raise SystemExit("upstream source checksum mismatch")
    env = dict(os.environ, CGO_ENABLED="0", GOTOOLCHAIN="go1.27.2", GOMAXPROCS="2",
               GOMEMLIMIT="3GiB", GOWORK="off", GOPROXY="https://proxy.golang.org",
               GOSUMDB="sum.golang.org", GONOSUMDB="", GONOPROXY="", GOPRIVATE="")
    toolchain = subprocess.check_output(["go", "env", "GOVERSION", "GOOS", "GOARCH"],
                                       env=env, text=True).splitlines()
    if toolchain != ["go1.27.2", "linux", metadata["architecture"]]:
        raise SystemExit("Go 1.27.2 and a native Linux target builder are required")
    unpacked = directory / "source"
    unpacked.mkdir()
    with tarfile.open(archive, "r:gz") as source:
        source.extractall(unpacked, filter="data")
    source = unpacked / f"inspektor-gadget-{COMMIT}"
    if (source / "go.mod").read_text().splitlines()[0] != "module github.com/inspektor-gadget/inspektor-gadget":
        raise SystemExit("unexpected upstream source module")
    for name in ("go.mod", "go.sum"):
        shutil.copyfile(directory / f"client.{name}", source / name)

    def go(*args):
        subprocess.run(["go", *args], cwd=source, env=env, check=True)

    go("test", "-mod=readonly", "-p=2", "-trimpath", "-tags=withoutebpf", "-count=1",
       "./cmd/kubectl-gadget", "./pkg/oci")
    go("test", "-mod=readonly", "-p=2", "-trimpath", "-count=1",
       "./gadget-container/gadgettracermanager", "./pkg/gadget-service/api-helpers")
    go("mod", "verify")
    binary = directory / "kubectl-gadget"
    go("build", "-mod=readonly", "-p=2", "-tags=withoutebpf", "-trimpath", "-buildvcs=false",
       f"-ldflags=-s -w -X github.com/inspektor-gadget/inspektor-gadget/internal/version.version={VERSION} -X main.gadgetimage={DAEMON} -extldflags=-static",
       "-o", str(binary), "./cmd/kubectl-gadget")
    buildinfo = subprocess.check_output(["go", "version", "-m", str(binary)], env=env)
    (directory / "kubectl-gadget.buildinfo").write_bytes(buildinfo)
    subprocess.run([str(binary), "--help"], env=env, check=True)
    daemon = directory / "gadgettracermanager"
    go("build", "-mod=readonly", "-p=2", "-trimpath", "-buildvcs=false",
       f"-ldflags=-s -w -X github.com/inspektor-gadget/inspektor-gadget/internal/version.version={VERSION}",
       "-o", str(daemon), "./gadget-container/gadgettracermanager")
    (directory / "gadgettracermanager.buildinfo").write_bytes(
        subprocess.check_output(["go", "version", "-m", str(daemon)], env=env))
    subprocess.run([str(daemon), "-h"], env=env, check=True)
    go("mod", "verify")
    for name in ("go.mod", "go.sum"):
        if digest(directory / f"client.{name}") != digest(source / name):
            raise SystemExit(f"reviewed client.{name} changed during build")
    shutil.copyfile(source / "LICENSE", directory / "LICENSE")
    binary.chmod(0o555)
    daemon.chmod(0o555)
    metadata.update(
        ig_version="v0.53.2", client_version=VERSION, daemon_version=VERSION, client_source_revision=COMMIT,
        client_source_archive_sha256=SOURCE_SHA256, go_version=toolchain[0],
        artifacts={name: digest(directory / name) for name in (
            "kubectl-gadget", "kubectl-gadget.buildinfo", "gadgettracermanager",
            "gadgettracermanager.buildinfo", "client.go.mod", "client.go.sum", "LICENSE")},
    )
    (directory / "build.json").write_text(json.dumps(metadata, indent=2) + "\n")
    package(directory)


if __name__ == "__main__":
    build(sys.argv[1])
