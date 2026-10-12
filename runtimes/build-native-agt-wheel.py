# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Build and inspect the pinned ACS Linux ABI3 wheel without target emulation."""
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import signal
import subprocess
import sys
import tempfile
import tomllib
import zipfile

TARGETS = {
    "amd64": ("x86_64-unknown-linux-gnu", "x86_64-linux-gnu", 62, "linux_x86_64"),
    "arm64": ("aarch64-unknown-linux-gnu", "aarch64-linux-gnu", 183, "linux_aarch64"),
}
RUST_VERSION = "1.95.0"
ACS_VERSION = "0.4.0b0"
COMPILE_SECONDS = 900


def require(condition, message):
    if not condition:
        raise ValueError(message)


def command(arguments, *, environment=None, timeout=30):
    process = subprocess.Popen(
        arguments, env=environment, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, start_new_session=True,
    )
    try:
        output, _ = process.communicate(timeout=timeout)
    except BaseException:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate(timeout=20)
        raise
    print(output, end="", flush=True)
    if process.returncode:
        raise subprocess.CalledProcessError(process.returncode, arguments, output=output)
    return output


def inspect_library(header, version_info, dynamic_info, machine):
    require(len(header) >= 20 and header[:6] == b"\x7fELF\x02\x01",
            "ACS extension must be ELF64 little-endian")
    require(int.from_bytes(header[18:20], "little") == machine,
            "ACS extension architecture differs from its wheel tag")
    require("GLIBC_PRIVATE" not in version_info, "Private GLIBC dependency")
    versions = {tuple(map(int, value.split(".")))
                for value in re.findall(r"\bGLIBC_([0-9]+(?:\.[0-9]+)+)\b", version_info)}
    require(bool(versions) and max(versions) <= (2, 36),
            "ACS extension exceeds the qualified GLIBC 2.36 baseline")
    require("(RPATH)" not in dynamic_info and "(RUNPATH)" not in dynamic_info,
            "ACS extension must not embed a builder library path")
    needed = set(re.findall(r"\(NEEDED\).*\[([^\]]+)\]", dynamic_info))
    allowed = {"libgcc_s.so.1", "libc.so.6", "libm.so.6", "libpthread.so.0",
               "librt.so.1", "libdl.so.2", "libutil.so.1"}
    allowed.add("ld-linux-aarch64.so.1" if machine == 183 else "ld-linux-x86-64.so.2")
    require(bool(needed) and needed <= allowed,
            "ACS extension has unqualified shared-library dependencies: " + repr(sorted(needed - allowed)))
    return {"elf_machine": machine, "glibc_max": ".".join(map(str, max(versions))),
            "needed": sorted(needed)}


def build(architecture):
    require(architecture in TARGETS, "Expected amd64 or arm64")
    require(sys.platform == "linux" and platform.machine() in ("x86_64", "aarch64"),
            "ACS builder must run on a supported Linux architecture")
    target, cross, machine, wheel_platform = TARGETS[architecture]
    project = tomllib.loads(Path("pyproject.toml").read_text())
    require(project["project"]["name"] == "agent-control-specification"
            and project["project"]["version"] == ACS_VERSION, "Unexpected ACS source version")
    cargo = tomllib.loads(Path("Cargo.toml").read_text())
    require("abi3-py311" in cargo["dependencies"]["pyo3"]["features"],
            "ACS source no longer provides the selected Python ABI3 baseline")
    lock = Path("../../Cargo.lock")
    before_lock = lock.read_bytes()
    require(not any(item["name"] == "openssl-sys"
                    for item in tomllib.loads(before_lock.decode())["package"]),
            "ACS source now needs an unqualified target OpenSSL sysroot")
    rust = command(["rustc", "-vV"])
    host = "x86_64" if platform.machine() == "x86_64" else "aarch64"
    require(f"release: {RUST_VERSION}\n" in rust
            and f"host: {host}-unknown-linux-gnu\n" in rust,
            "Compiler is not the selected native Rust toolchain")
    require(command(["cargo", "--version"]).startswith(f"cargo {RUST_VERSION} "),
            "Unexpected Cargo version")
    require(command(["maturin", "--version"]).strip() == "maturin 1.8.7",
            "Unexpected maturin version")
    require(command([cross + "-gcc", "-dumpmachine"]).strip() == cross,
            "Cross linker target differs")
    forbidden = ("PYO3_CONFIG_FILE", "PYO3_CROSS_LIB_DIR", "PYO3_PYTHON",
                 "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET",
                 "PKG_CONFIG_PATH", "PKG_CONFIG_LIBDIR", "PKG_CONFIG_ALLOW_CROSS")
    require(not any(os.environ.get(name) for name in forbidden),
            "Unreviewed native build override")
    environment = {
        **os.environ,
        f"CARGO_TARGET_{target.upper().replace('-', '_')}_LINKER": cross + "-gcc",
        f"CC_{target.replace('-', '_')}": cross + "-gcc",
        f"CXX_{target.replace('-', '_')}": cross + "-g++",
        f"AR_{target.replace('-', '_')}": cross + "-ar",
        "PYO3_CROSS": "1", "PYO3_CROSS_PYTHON_VERSION": "3.12",
        "PYO3_CROSS_PYTHON_IMPLEMENTATION": "CPython",
    }
    output = Path("/wheels") / architecture
    output.mkdir(parents=True)
    try:
        command(["maturin", "build", "--release", "--locked", "--target", target,
                 "--compatibility", "linux", "--out", str(output)],
                environment=environment, timeout=COMPILE_SECONDS)
    finally:
        require(lock.read_bytes() == before_lock, "ACS build changed the pinned Cargo lock")
    wheel_name = f"agent_control_specification-{ACS_VERSION}-cp311-abi3-{wheel_platform}.whl"
    require([path.name for path in output.iterdir()] == [wheel_name],
            "ACS build did not emit exactly the selected ABI3 wheel")
    wheel = output / wheel_name
    with zipfile.ZipFile(wheel) as archive:
        names = [name for name in archive.namelist() if name.endswith(".so")]
        require(names == ["agent_control_specification/_native.abi3.so"],
                "Unexpected ACS native extension layout")
        library = archive.read(names[0])
    with tempfile.TemporaryDirectory(prefix="acs-inspect-") as directory:
        path = Path(directory) / "native.so"
        path.write_bytes(library)
        details = inspect_library(
            library[:20],
            command([cross + "-readelf", "--version-info", "--wide", str(path)]),
            command([cross + "-readelf", "--dynamic", "--wide", str(path)]), machine,
        )
    receipt = {
        **details, "target": target, "rust": RUST_VERSION, "maturin": "1.8.7",
        "wheel": wheel_name, "sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
        "cargo_lock_sha256": hashlib.sha256(before_lock).hexdigest(),
        "native_runtime_acceptance": False,
    }
    (output / "build.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt), flush=True)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: build-native-agt-wheel.py amd64|arm64")
    build(sys.argv[1])
