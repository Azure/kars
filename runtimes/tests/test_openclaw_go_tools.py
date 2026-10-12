# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise build orchestration; compiler doubles do not qualify real binaries."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "sandbox-images/openclaw/build-go-tools.sh"
TOOLS = {"blu", "eightctl", "gifgrep", "ordercli", "sonos", "camsnap",
         "goplaces", "songsee", "wacli"}
TOOL_DOUBLE = r"""
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps({'tool': name, 'args': args,
        'env': {key: os.environ.get(key) for key in
                ('GOOS', 'GOARCH', 'GOHOSTARCH', 'CC', 'CGO_ENABLED', 'GOBIN', 'GOMAXPROCS')}}) + '\n')
if name == 'go':
    if args[0] == 'env':
        print({'GOHOSTOS': 'linux', 'GOHOSTARCH': os.environ['GOHOSTARCH'],
               'GOPATH': os.environ['GOPATH']}[args[1]])
    else:
        tool = args[-1].split('@')[0].rsplit('/', 1)[1]
        if tool == os.environ.get('FAIL_TOOL'):
            sys.exit(23)
        if tool != os.environ.get('OMIT_TOOL'):
            dest = pathlib.Path(os.environ['GOPATH']) / 'bin'
            if os.environ['GOARCH'] != os.environ['GOHOSTARCH']:
                dest /= 'linux_' + os.environ['GOARCH']
            dest.mkdir(parents=True, exist_ok=True)
            dest /= tool
            dest.write_text('test compiler output\n')
            dest.chmod(0o755)
elif name == 'readelf':
    if os.environ.get('BAD_ELF') == 'error':
        sys.exit(24)
    if args[0] == '-h':
        machine = {'arm64': 'AArch64', 'amd64': 'Advanced Micro Devices X86-64'}[os.environ['GOARCH']]
        print('  Machine: ' + ('wrong' if os.environ.get('BAD_ELF') == 'machine' else machine))
    elif args[0] == '-l' and os.environ.get('BAD_ELF') == 'loader':
        print(' INTERP ')
    elif args[0] == '-d' and os.environ.get('BAD_ELF') == 'shared':
        print(' (NEEDED) Shared library: [libc.so.6]')
"""


class OpenClawGoToolsTests(unittest.TestCase):
    def run_build(self, target, host="amd64", **extra):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            commands = work / "commands"
            commands.mkdir()
            for tool in ("go", "apt-get", "readelf", "rm"):
                executable = commands / tool
                executable.write_text(f"#!{sys.executable}\n" + TOOL_DOUBLE)
                executable.chmod(0o755)
            calls = work / "calls.jsonl"
            output = work / "output"
            env = {
                **os.environ, "PATH": f"{commands}:/usr/bin:/bin",
                "CALLS": str(calls), "GOPATH": str(work / "gopath"),
                "GOHOSTARCH": host, "GOBIN": "/must/not/be/used", **extra,
            }
            result = subprocess.run(
                ["/bin/sh", str(SCRIPT), target, str(output)],
                env=env, capture_output=True, text=True, timeout=15,
            )
            records = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            outputs = {path.name for path in output.iterdir()} if output.exists() else set()
            return result, records, outputs

    def test_native_and_cross_builds_keep_all_nine_tools(self):
        for host in ("amd64", "arm64"):
            for target in ("amd64", "arm64"):
                with self.subTest(host=host, target=target):
                    result, calls, outputs = self.run_build(target, host)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(outputs, TOOLS)
                    installs = [c for c in calls if c["tool"] == "go" and c["args"][0] == "install"]
                    self.assertEqual(len(installs), 9)
                    for call in installs:
                        self.assertEqual(call["args"][1:3], ["-p", "2"])
                        self.assertEqual(call["env"]["GOARCH"], target)
                        self.assertEqual(call["env"]["GOOS"], "linux")
                        self.assertEqual(call["env"]["GOMAXPROCS"], "2")
                        self.assertIsNone(call["env"]["GOBIN"])
                    self.assertTrue(all(c["env"]["CGO_ENABLED"] == "0" for c in installs[:8]))
                    wacli = installs[-1]
                    self.assertEqual(wacli["env"]["CGO_ENABLED"], "1")
                    triple = "aarch64-linux-gnu" if target == "arm64" else "x86_64-linux-gnu"
                    self.assertEqual(wacli["env"]["CC"], "gcc" if host == target else triple + "-gcc")
                    self.assertIn('sqlite_omit_load_extension,netgo,osusergo', wacli["args"])
                    self.assertIn('-ldflags=-linkmode external -extldflags "-static"', wacli["args"])
                    packages = next(c["args"] for c in calls if c["tool"] == "apt-get" and c["args"][0] == "install")
                    self.assertIn("gcc" if host == target else "gcc-" + triple, packages)
                    self.assertIn("libc6-dev" if host == target else "libc6-dev-" + target + "-cross", packages)

    def test_invalid_targets_fail_before_package_installation(self):
        for target in ("", "s390x", "x86_64", "arm64; exit 0"):
            with self.subTest(target=target):
                result, calls, outputs = self.run_build(target)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Unsupported Go tools target architecture", result.stderr)
                self.assertEqual(calls, [])
                self.assertEqual(outputs, set())

    def test_failed_compiler_is_not_ignored(self):
        result, _, outputs = self.run_build("arm64", FAIL_TOOL="blu")
        self.assertEqual(result.returncode, 23)
        self.assertEqual(outputs, set())

    def test_missing_executable_fails_the_build(self):
        result, _, outputs = self.run_build("arm64", OMIT_TOOL="blu")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(outputs, set())

    def test_wrong_architecture_dynamic_linkage_and_inspection_errors_fail(self):
        for bad in ("machine", "loader", "shared", "error"):
            with self.subTest(bad=bad):
                result, _, outputs = self.run_build("arm64", BAD_ELF=bad)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(outputs, set())

    def test_recipe_builds_tools_natively_before_target_stages(self):
        text = (ROOT / "sandbox-images/openclaw/Dockerfile.base").read_text()
        first_stage = next(line for line in text.splitlines() if line.startswith("FROM "))
        self.assertIn("--platform=${BUILDPLATFORM}", first_stage)
        self.assertIn("golang:1.26.6-bookworm@sha256:", first_stage)
        self.assertTrue(first_stage.endswith(" AS go-builder"))
        self.assertIn('RUN sh /tmp/build-go-tools.sh "$TARGETARCH" /out', text)
        self.assertIn("COPY --from=go-builder /out/ /usr/local/bin/", text)
        subprocess.run(["/bin/sh", "-n", str(SCRIPT)], check=True, capture_output=True)


if __name__ == "__main__":
    unittest.main()
