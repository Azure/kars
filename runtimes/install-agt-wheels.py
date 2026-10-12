# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Validate the complete pinned AGT owner set before installing image build inputs."""
from email.parser import BytesParser
import argparse
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import sys
import zipfile

OWNERS = ("agent_governance_toolkit_core", "agent_governance_toolkit_cli", "a2a_agentmesh")
PLATFORMS = {"x86_64": ("amd64", 62), "aarch64": ("arm64", 183)}


def select_wheels(root, machine):
    if machine not in PLATFORMS:
        raise ValueError("AGT wheels support Linux amd64 and arm64 only")
    architecture, elf_machine = PLATFORMS[machine]
    expected = {f"{name}-5.0.0-py3-none-any.whl" for name in OWNERS}
    actual = {path.name for path in root.glob("*.whl")}
    if actual != expected:
        raise ValueError(f"Expected exactly the AGT 5 core, CLI and A2A wheels; got {sorted(actual)}")
    wheels = [root / name for name in sorted(expected)]
    native = root / architecture
    name = f"agent_control_specification-0.4.0b0-cp311-abi3-linux_{machine}.whl"
    receipt_path = native / "build.json"
    if native.is_symlink() or receipt_path.is_symlink() or not receipt_path.is_file():
        raise ValueError("Missing regular native ACS build receipt")
    receipt = json.loads(receipt_path.read_text())
    wheel = native / name
    if wheel.is_symlink() or not wheel.is_file():
        raise ValueError("Missing regular architecture-matched ACS wheel")
    if (receipt.get("wheel") != name or receipt.get("elf_machine") != elf_machine
            or receipt.get("target") != f"{machine}-unknown-linux-gnu"
            or receipt.get("rust") != "1.95.0" or receipt.get("maturin") != "1.8.7"
            or receipt.get("sha256") != hashlib.sha256(wheel.read_bytes()).hexdigest()):
        raise ValueError("Native ACS wheel identity or build receipt differs")
    wheels.append(wheel)
    for path in wheels:
        if path.is_symlink() or not path.is_file():
            raise ValueError("AGT wheel must be a regular file: " + str(path))
        with zipfile.ZipFile(path) as archive:
            names = [name for name in archive.namelist() if name.endswith(".dist-info/METADATA")]
            if len(names) != 1:
                raise ValueError("Ambiguous AGT wheel metadata: " + str(path))
            metadata = BytesParser().parsebytes(archive.read(names[0]))
            expected_name, expected_version = path.name.split("-", 2)[:2]
            if (metadata["Name"].replace("-", "_").lower() != expected_name
                    or metadata["Version"] != expected_version):
                raise ValueError("AGT wheel metadata differs from its selected identity: " + str(path))
    return wheels


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--validate", type=Path, help="Check both architectures without installing")
    mode.add_argument("--constraint", type=Path, help="Apply the runtime's locked dependency constraints")
    options = parser.parse_args()
    if options.validate:
        for machine in PLATFORMS:
            select_wheels(options.validate, machine)
    else:
        wheels = select_wheels(Path("/tmp/agt-wheels"), platform.machine())
        constraints = ["--constraint", str(options.constraint)] if options.constraint else []
        subprocess.run([sys.executable, "-m", "pip", "install", "--no-cache-dir",
                        *constraints, *(str(path) for path in wheels)], check=True)
