# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os
import contextlib
import hashlib
import io
import json
import pathlib
import re
import runpy
import shlex
import signal
import subprocess
import sys
import tempfile
import unittest
import zipfile


ROOT = pathlib.Path(__file__).resolve().parents[2]
PACKAGES = {
    "agent-governance-toolkit-core": "agent_governance_toolkit_core",
    "agent-governance-toolkit-cli": "agent_governance_toolkit_cli",
    "agentmesh-integrations/a2a-protocol": "a2a_agentmesh",
}
SELECT = runpy.run_path(str(ROOT / "runtimes/install-agt-wheels.py"))["select_wheels"]
NATIVE = runpy.run_path(str(ROOT / "runtimes/build-native-agt-wheel.py"))
INSPECT = NATIVE["inspect_library"]


def wheel(path, name, version):
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(f"{name}-{version}.dist-info/METADATA",
                         f"Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n")


def fixture_wheels(root):
    for name in PACKAGES.values():
        wheel(root / f"{name}-5.0.0-py3-none-any.whl", name, "5.0.0")
    for architecture, machine, elf in (("amd64", "x86_64", 62), ("arm64", "aarch64", 183)):
        native = root / architecture
        native.mkdir()
        path = native / f"agent_control_specification-0.4.0b0-cp311-abi3-linux_{machine}.whl"
        wheel(path, "agent_control_specification", "0.4.0b0")
        (native / "build.json").write_text(json.dumps({
            "wheel": path.name, "elf_machine": elf, "target": f"{machine}-unknown-linux-gnu",
            "rust": "1.95.0", "maturin": "1.8.7", "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }))


class AgtWheelPackagingTests(unittest.TestCase):
    def test_native_command_preserves_output_and_nonzero_exit(self):
        with contextlib.redirect_stdout(io.StringIO()) as log:
            self.assertEqual(NATIVE["command"]([sys.executable, "-c", "print('native output')"]),
                             "native output\n")
            with self.assertRaises(subprocess.CalledProcessError) as failure:
                NATIVE["command"]([sys.executable, "-c", "print('native failure'); raise SystemExit(23)"])
        self.assertEqual(failure.exception.returncode, 23)
        self.assertEqual(failure.exception.output, "native failure\n")
        self.assertEqual(log.getvalue(), "native output\nnative failure\n")

    def test_native_command_timeout_kills_owned_group_and_reaps_leader(self):
        for leader_exits in (False, True):
            with self.subTest(leader_exits=leader_exits), tempfile.TemporaryDirectory() as directory:
                identities = pathlib.Path(directory) / "pids.json"
                program = (
                    "import json, os, pathlib, subprocess, sys, time\n"
                    "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])\n"
                    "pathlib.Path(sys.argv[1]).write_text(json.dumps([os.getpid(), child.pid]))\n"
                    "if sys.argv[2] == 'False': time.sleep(60)\n"
                )
                try:
                    with self.assertRaises(subprocess.TimeoutExpired):
                        NATIVE["command"]([sys.executable, "-c", program, str(identities),
                                           str(leader_exits)], timeout=2)
                    parent, child = json.loads(identities.read_text())
                    with self.assertRaises(ProcessLookupError):
                        os.kill(parent, 0)
                    state = subprocess.run(["ps", "-p", str(child), "-o", "stat="],
                                           capture_output=True, text=True, timeout=5)
                    self.assertIn(state.returncode, (0, 1), state.stderr)
                    self.assertTrue(not state.stdout.strip() or state.stdout.strip().startswith("Z"),
                                    "Owned descendant survived command timeout")
                finally:
                    if identities.exists():
                        parent, _ = json.loads(identities.read_text())
                        try:
                            os.killpg(parent, signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def test_direct_rebuild_invalidates_old_pin_even_after_last_wheel_failure(self):
        for build_exit, native_exit, source_state in (
            (0, 0, "clean"), (42, 0, "clean"), (0, 43, "clean"),
            (0, 0, "dirty"), (0, 0, "wrong-pin"),
        ):
            with self.subTest(build_exit=build_exit, native_exit=native_exit, source=source_state), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                runtimes = root / "runtimes"
                wheels = runtimes / "wheels"
                wheels.mkdir(parents=True)
                stamp = wheels / ".agt-sha"
                stamp.write_text("a" * 40 + "\n")
                (wheels / "stale-1.0.0-py3-none-any.whl").touch()
                for architecture in ("amd64", "arm64"):
                    (wheels / architecture).mkdir()
                    (wheels / architecture / "stale.whl").touch()
                upstream = root / "upstream"
                source = upstream / "agent-governance-python"
                for package in PACKAGES:
                    project = source / package / "pyproject.toml"
                    project.parent.mkdir(parents=True)
                    project.write_text("[project]\n")
                script = runtimes / "build-agt-wheels.sh"
                script.write_text((ROOT / "runtimes/build-agt-wheels.sh").read_text())
                (runtimes / "install-agt-wheels.py").write_text((ROOT / "runtimes/install-agt-wheels.py").read_text())
                project = upstream / "policy-engine/sdk/python/pyproject.toml"
                project.parent.mkdir(parents=True)
                project.touch()
                (upstream / ".gitignore").write_text("policy-engine/target/\n")
                for args in (["init", "--quiet"], ["add", "."],
                             ["-c", "user.name=Kars test", "-c", "user.email=test@example.invalid",
                              "-c", "core.hooksPath=/dev/null", "commit", "--quiet", "-m", "fixture"]):
                    subprocess.run(["git", *args], cwd=upstream, check=True, capture_output=True)
                pin = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=upstream, text=True).strip()
                pinfile = root / "vendor/agt/pin.json"
                pinfile.parent.mkdir(parents=True)
                pinfile.write_text(json.dumps({"sha": pin}))
                ignored = upstream / "policy-engine/target"
                ignored.mkdir()
                (ignored / "stale-build").write_text("must not enter the compiler context")
                if source_state == "dirty":
                    project.write_text("user change\n")
                elif source_state == "wrong-pin":
                    pinfile.write_text(json.dumps({"sha": "0" * 40}))
                native = root / "native-fixtures"
                native.mkdir()
                fixture_wheels(native)
                binaries = root / "bin"
                binaries.mkdir()
                docker = binaries / "docker"
                docker.write_text(
                    "#!/bin/sh\nset -eu\n"
                    '[ "$NATIVE_EXIT" = 0 ] || exit "$NATIVE_EXIT"\n'
                    'for arg in "$@"; do case "$arg" in type=local,dest=*) output="${arg#type=local,dest=}" ;; agt=*) source="${arg#agt=}" ;; esac; done\n'
                    'test -f "$source/policy-engine/sdk/python/pyproject.toml"\n'
                    'test ! -e "$source/policy-engine/target"\n'
                    'cp -R "$NATIVE_FIXTURES/amd64" "$output/amd64"\n'
                    'cp -R "$NATIVE_FIXTURES/arm64" "$output/arm64"\n'
                )
                docker.chmod(0o755)
                python = runtimes / ".builder-venv/bin/python"
                python.parent.mkdir(parents=True)
                python.write_text(
                    "#!/bin/sh\n"
                    'if [ "$1" = -m ] && [ "$2" = pip ]; then exit 0; fi\n'
                    '[ "$1" = -m ] && [ "$2" = build ] || exec "$REAL_PYTHON" "$@"\n'
                    'case "$6" in\n'
                    + "".join(
                        f"  */{source_name}) package={wheel_name} ;;\n"
                        for source_name, wheel_name in PACKAGES.items()
                    )
                    + "  *) exit 98 ;;\n"
                    "esac\n"
                    'cp "$NATIVE_FIXTURES/${package}-5.0.0-py3-none-any.whl" "$5/"\n'
                    'if [ "$package" = a2a_agentmesh ]; then exit "$BUILD_EXIT"; fi\n'
                )
                python.chmod(0o755)
                result = subprocess.run(
                    ["bash", str(script)],
                    env={**os.environ, "AGT_PYTHON_DIR": str(source), "BUILD_EXIT": str(build_exit),
                         "NATIVE_EXIT": str(native_exit), "REAL_PYTHON": sys.executable, "PYTHON": sys.executable,
                         "NATIVE_FIXTURES": str(native), "PATH": str(binaries) + os.pathsep + os.environ["PATH"]},
                    capture_output=True,
                    text=True,
                )
                if source_state != "clean":
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertIn("clean checkout", result.stderr)
                    self.assertEqual(stamp.read_text(), "a" * 40 + "\n")
                    self.assertTrue((wheels / "stale-1.0.0-py3-none-any.whl").exists())
                    continue
                self.assertEqual(result.returncode, build_exit or native_exit, result.stderr)
                self.assertEqual(
                    {p.name for p in wheels.glob("*.whl")},
                    {f"{package}-5.0.0-py3-none-any.whl" for package in PACKAGES.values()},
                )
                self.assertFalse(stamp.exists(), "Replacement wheels retain the previous source pin")
                self.assertEqual(list(runtimes.glob(".native-wheels.*")), [])
                for architecture in ("amd64", "arm64"):
                    self.assertFalse((wheels / architecture / "stale.whl").exists())
                    self.assertEqual((wheels / architecture).exists(), not (build_exit or native_exit))

    def test_builder_includes_consolidated_dependencies(self):
        script = (ROOT / "runtimes/build-agt-wheels.sh").read_text()
        packages = re.search(r"PACKAGES=\((.*?)\)", script, re.S)
        self.assertIsNotNone(packages)
        self.assertEqual(set(shlex.split(packages.group(1))), set(PACKAGES))

    def test_all_python_images_use_the_shared_fail_closed_installer(self):
        for runtime in ("hermes", "anthropic", "langgraph", "maf-python", "openai-agents", "pydantic-ai"):
            dockerfile = (ROOT / f"sandbox-images/{runtime}/Dockerfile").read_text()
            self.assertIn("COPY runtimes/install-agt-wheels.py /tmp/install-agt-wheels.py", dockerfile)
            self.assertIn("RUN python3 /tmp/install-agt-wheels.py", dockerfile)
            self.assertNotIn("if ls /tmp/agt-wheels", dockerfile)

    def test_image_accepts_both_complete_architectures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            fixture_wheels(root)
            for machine in ("x86_64", "aarch64"):
                selected = SELECT(root, machine)
                self.assertEqual(len(selected), 4)
                self.assertTrue(selected[-1].name.endswith(f"linux_{machine}.whl"))

    def test_image_rejects_missing_legacy_or_mutated_inputs(self):
        cases = ("empty", "legacy", "native-missing", "hash", "metadata", "symlink", "target", "rust", *PACKAGES.values())
        for case in cases:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                fixture_wheels(root)
                native = next((root / "amd64").glob("*.whl"))
                receipt = root / "amd64/build.json"
                pure = root / "a2a_agentmesh-5.0.0-py3-none-any.whl"
                if case == "empty":
                    for path in root.glob("*.whl"):
                        path.unlink()
                elif case == "legacy":
                    (root / "agentmesh_platform-4.0.0-py3-none-any.whl").touch()
                elif case == "native-missing":
                    native.unlink()
                elif case == "hash":
                    native.write_bytes(b"modified")
                elif case == "metadata":
                    wheel(pure, "a2a_agentmesh", "4.0.0")
                elif case == "symlink":
                    pure.unlink()
                    pure.symlink_to(native)
                elif case in ("target", "rust"):
                    data = json.loads(receipt.read_text())
                    data[case] = "wrong"
                    receipt.write_text(json.dumps(data))
                else:
                    (root / f"{case}-5.0.0-py3-none-any.whl").unlink()
                with self.assertRaises(ValueError):
                    SELECT(root, "x86_64")

    def test_native_abi_checks_both_targets_and_rejects_drift(self):
        for machine in (62, 183):
            header = b"\x7fELF\x02\x01" + b"\x00" * 12 + machine.to_bytes(2, "little")
            versions = "GLIBC_2.17 GLIBC_2.36"
            dynamic = " 0x1 (NEEDED) Shared library: [libc.so.6]"
            self.assertEqual(INSPECT(header, versions, dynamic, machine)["elf_machine"], machine)
            for mutated in (
                (b"bad", versions, dynamic, machine),
                (header, versions, dynamic, 183 if machine == 62 else 62),
                (header, "GLIBC_2.37", dynamic, machine),
                (header, "GLIBC_PRIVATE", dynamic, machine),
                (header, "", dynamic, machine),
                (header, versions, dynamic + "\n(RUNPATH) [/builder]", machine),
                (header, versions, "(NEEDED) Shared library: [libssl.so.3]", machine),
                (header, versions, "", machine),
            ):
                with self.subTest(machine=machine, mutated=mutated), self.assertRaises(ValueError):
                    INSPECT(*mutated)


if __name__ == "__main__":
    unittest.main()
