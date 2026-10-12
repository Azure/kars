# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Lightweight Hermes image-input guards; these do not qualify built images."""

from __future__ import annotations

import io
import hashlib
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tarfile
import unittest
import uuid


ROOT = Path(__file__).resolve().parents[3]
IMAGE = ROOT / "sandbox-images/hermes"
DOCKERFILE = (IMAGE / "Dockerfile").read_text()
RUNS = re.findall(r"^RUN (.*?)(?=\n[^ \t]|\Z)", DOCKERFILE, re.M | re.S)
NODE_RUN = next(run for run in RUNS if 'NODE_DIR="node-v' in run).replace("\\\n", " ")
NODE_VERSION = re.search(r"^ARG NODE22_VERSION=(\S+)$", DOCKERFILE, re.M).group(1)


class HermesImageDependenciesTests(unittest.TestCase):
    def test_genuine_source_and_security_inputs_are_bound(self):
        names = set()
        for line in (IMAGE / "hermes-source-SHA256SUMS").read_text().splitlines():
            digest, name = line.split()
            self.assertEqual(Path(name).name, name)
            self.assertNotIn(name, names)
            names.add(name)
            self.assertEqual(hashlib.sha256((IMAGE / name).read_bytes()).hexdigest(), digest)
        self.assertEqual(names, {
            "hermes-dependency-fixes.patch", "hermes-uv.lock", "hermes-core-constraints.txt",
        })
        run = next(run for run in RUNS if "git init -q /opt/hermes" in run)
        self.assertIn("ARG HERMES_COMMIT=f97608f178d1ffeca59860195ab7da295f7c8e5f", DOCKERFILE)
        self.assertIn("1a98d5528c88ddc3d4e32439d3e8322487ce31bc", run)
        self.assertIn("2c87202b1566e4daed16b1472bc5d5e3651af9f6fd64acc953412f43432fefb0", run)
        for digest in (
            "6dd85804ac28a0f2d326030e69c4f65a9ae8968478af87d2267d6ad89cc7230a",
            "2eee7bf49d29e6d0b218dcc960a174b99cc177039f6941f0355ebff9a93e33f7",
            "14c2e288f07d4b6290946c1d0031fce51d7ab466e11225c8acbcd4331fc7b846",
        ):
            self.assertIn(digest, run)
        self.assertIn("--editable /opt/hermes", run)
        self.assertIn("direct_url.json", run)
        self.assertIn("file:///opt/hermes", run)
        self.assertLess(run.index("sha256sum"), run.index("pip install"))
        for install in (run for run in RUNS if "pip install" in run):
            self.assertIn("--constraint /opt/hermes-build/hermes-core-constraints.txt", install)

    def test_python_security_pins_match_lock_and_constraints(self):
        patch = (IMAGE / "hermes-dependency-fixes.patch").read_text()
        lock = (IMAGE / "hermes-uv.lock").read_text()
        constraints = (IMAGE / "hermes-core-constraints.txt").read_text().splitlines()
        for name, version in (("anyio", "4.14.2"), ("urllib3", "2.8.0")):
            with self.subTest(package=name):
                self.assertIn(f'+  "{name}=={version}"', patch)
                self.assertIn(f"+{name} = false", patch)
                self.assertRegex(
                    lock, rf'(?m)^name = "{name}"\nversion = "{re.escape(version)}"$',
                )
                self.assertIn(f"{name}=={version}", constraints)

    def test_ui_builds_locked_workspaces_and_selects_prebuilt_paths(self):
        run = next(run for run in RUNS if "npm ci" in run)
        self.assertIn("--workspace ui-tui --workspace web --include-workspace-root", run)
        self.assertIn("--ignore-scripts --include=dev", run)
        self.assertIn("(cd ui-tui && node -e", run)
        self.assertIn("require('undici/package.json').version, '6.28.1'", run)
        self.assertLess(run.index("require('undici/package.json')"), run.index("npm run build"))
        for workspace in ("ui-tui", "web"):
            self.assertIn(f"npm run build --workspace {workspace}", run)
        self.assertIn("test -s ui-tui/dist/entry.js", run)
        self.assertIn("test -s hermes_cli/web_dist/index.html", run)
        self.assertIn("HERMES_TUI_DIR=/opt/hermes/ui-tui", DOCKERFILE)
        self.assertNotIn("HERMES_WEB_DIST=", DOCKERFILE)
        self.assertLess(run.index("npm ci"), run.index("npm run build"))
        self.assertLess(run.index("test -s hermes_cli/web_dist"), run.index("rm -rf"))

    def test_base_stays_digest_pinned_and_refreshes_supported_updates(self):
        self.assertRegex(
            DOCKERFILE,
            r"FROM mcr\.microsoft\.com/azurelinux/base/python:3\.12@sha256:[0-9a-f]{64} AS base",
        )
        system_run = next(run for run in RUNS if "tdnf install" in run)
        self.assertIn("tdnf update -y --refresh", system_run)
        self.assertLess(system_run.index("tdnf update"), system_run.index("tdnf install"))
        self.assertIn("nodejs nodejs-npm", system_run)
        self.assertIn("tdnf clean all", system_run)

    def test_node_checksums_cover_both_supported_architectures(self):
        entries = {}
        for line in (IMAGE / "node22-SHASUMS256.txt").read_text().splitlines():
            digest, name = line.split()
            self.assertRegex(digest, r"^[0-9a-f]{64}$")
            self.assertNotIn(name, entries)
            entries[name] = digest
        self.assertEqual(
            set(entries),
            {f"node-v{NODE_VERSION}-linux-{arch}.tar.xz" for arch in ("arm64", "x64")},
        )
        self.assertEqual(len(set(entries.values())), 2)

    def test_node_archive_is_verified_before_selective_extraction(self):
        self.assertIn("curl -fsSL", NODE_RUN)
        self.assertIn("sha256sum --check --ignore-missing /opt/node22-SHASUMS256.txt", NODE_RUN)
        self.assertLess(NODE_RUN.index("sha256sum"), NODE_RUN.index("tar -xJf"))
        self.assertIn("/opt/node22/bin/node --version", NODE_RUN)

    def test_extraction_keeps_executable_and_license_without_duplicate_npm(self):
        extract = next(part.strip() for part in NODE_RUN.split("&&") if part.strip().startswith("tar "))
        for arch in ("arm64", "x64"):
            with self.subTest(arch=arch):
                work = ROOT / f".hermes-image-input-test-{uuid.uuid4().hex}"
                work.mkdir()
                self.addCleanup(shutil.rmtree, work)
                node_dir = f"node-v{NODE_VERSION}-linux-{arch}"
                files = {
                    "bin/node": b"executable fixture",
                    "LICENSE": b"license fixture",
                    "lib/node_modules/npm/package.json": b"{}",
                }
                with tarfile.open(work / f"{node_dir}.tar.xz", "w:xz") as archive:
                    for name, content in files.items():
                        info = tarfile.TarInfo(f"{node_dir}/{name}")
                        info.size = len(content)
                        info.mode = 0o755 if name == "bin/node" else 0o644
                        archive.addfile(info, io.BytesIO(content))
                subprocess.run(
                    ["sh", "-c", extract],
                    cwd=work,
                    env={**os.environ, "NODE_DIR": node_dir},
                    check=True,
                    capture_output=True,
                )
                self.assertEqual((work / "bin/node").read_bytes(), files["bin/node"])
                self.assertEqual((work / "LICENSE").read_bytes(), files["LICENSE"])
                self.assertTrue(os.access(work / "bin/node", os.X_OK))
                self.assertFalse((work / "lib").exists())
                self.assertFalse((work / "bin/npm").exists())

    def test_tui_override_preserves_system_node_and_npm_on_path(self):
        entrypoint = (IMAGE / "entrypoint.sh").read_text()
        self.assertIn("export HERMES_NODE=/opt/node22/bin/node", entrypoint)
        self.assertNotRegex(entrypoint + DOCKERFILE, r"PATH=[^\n]*\/opt\/node22")

    def test_python_dependencies_are_checked_without_forcing_incompatible_versions(self):
        installs = [run for run in RUNS if "pip install" in run]
        last_install = installs[-1]
        self.assertIn("&& pip check", last_install)
        self.assertIn("--no-index --no-deps --force-reinstall", last_install)
        self.assertIn("/tmp/kars-adapter-wheels/*.whl", last_install)
        for install in installs[:-1]:
            self.assertNotIn("--no-deps", install)
        self.assertNotIn("--ignore-installed", DOCKERFILE)

    def test_adapter_refresh_uses_one_offline_runtime_install_and_removes_stale_code(self):
        self.assertIn("ARG HERMES_RUNTIME_BASE=base", DOCKERFILE)
        builder, runtime = DOCKERFILE.split("FROM ${HERMES_RUNTIME_BASE} AS runtime")
        self.assertIn("FROM base AS adapter-build\nUSER 0", builder)
        self.assertIn("pip wheel --no-cache-dir --no-deps", builder)
        self.assertIn("COPY --from=adapter-build /opt/kars-adapter-wheels/", runtime)
        self.assertEqual(runtime.count("pip install"), 1)
        self.assertLess(
            runtime.index("rm -rf /opt/kars-runtime-hermes /opt/kars-hermes-stage/plugins/kars"),
            runtime.index("COPY runtimes/hermes/pyproject.toml"),
        )
        self.assertIn("COPY runtimes/hermes/src/ /opt/kars-runtime-hermes/src/", runtime)
        self.assertNotIn("COPY runtimes/hermes/ ", DOCKERFILE)
        ignored = (IMAGE / "Dockerfile.dockerignore").read_text().splitlines()
        for pattern in ("**/.venv/", "**/__pycache__/", "**/*.pyc", "**/node_modules/"):
            self.assertIn(pattern, ignored)
        self.assertIn(
            "COPY runtimes/hermes/src/kars_runtime_hermes/plugin/ /opt/kars-hermes-stage/plugins/kars/",
            runtime,
        )
        self.assertIn("COPY sandbox-images/hermes/entrypoint.sh", runtime)
        self.assertEqual(runtime.strip().splitlines()[-1], "USER 1000")

    def test_slack_installs_and_checks_the_async_adapter_dependencies(self):
        run = next(run for run in RUNS if '"python-telegram-bot' in run)
        install, smoke = run.replace("\\\n", " ").split("&&")
        requirements = shlex.split(install)
        self.assertIn("slack-bolt==1.30.0", requirements)
        self.assertIn("slack-sdk>=3,<4", requirements)
        self.assertIn("aiohttp>=3.14.3,<4", requirements)
        command = shlex.split(smoke)
        self.assertEqual(command[:2], ["python3", "-c"])
        self.assertEqual(
            command[2].split("; "),
            [
                "from slack_bolt.async_app import AsyncApp",
                "from slack_bolt.adapter.socket_mode.async_handler import AsyncSocketModeHandler",
                "from slack_sdk.web.async_client import AsyncWebClient",
            ],
        )

    def test_slack_dependency_or_import_failure_stops_the_build(self):
        run = next(run for run in RUNS if '"python-telegram-bot' in run)
        for install_status, import_status in [(41, 0), (0, 42)]:
            with self.subTest(install_status=install_status, import_status=import_status):
                functions = (
                    f"pip() {{ return {install_status}; }}; "
                    f"python3() {{ printf 'import attempted'; return {import_status}; }}; "
                )
                result = subprocess.run(
                    ["sh", "-c", functions + run], text=True, capture_output=True,
                )
                self.assertEqual(result.returncode, install_status or import_status)
                self.assertEqual(result.stdout, "" if install_status else "import attempted")

    def test_all_build_shell_commands_parse(self):
        for run in RUNS:
            with self.subTest(command=run.splitlines()[0]):
                subprocess.run(["sh", "-n", "-c", run], check=True, capture_output=True)


if __name__ == "__main__":
    unittest.main()
