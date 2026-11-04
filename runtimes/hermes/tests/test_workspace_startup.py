# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute workspace initialization without starting Hermes or privileged setup."""

import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]
ENTRYPOINT = (ROOT / "sandbox-images/hermes/entrypoint.sh").read_text()
BLOCK = ENTRYPOINT.split("# ── Writable default workspace", 1)[1].split(
    "# ── SANDBOX_NAME fallback", 1
)[0]
BLOCK = "# ── Writable default workspace" + BLOCK


@unittest.skipIf(os.geteuid() == 0, "Filesystem permission tests require a non-root runner")
class WorkspaceStartupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.parent = Path(self.temp.name).resolve()
        self.agent = self.parent / "agent"
        self.agent.mkdir()
        self.addCleanup(lambda: self.agent.chmod(0o700) if self.agent.exists() else None)

    def run_block(self, *, cwd=None, as_sandbox=False, suffix=""):
        # Map only the image's fixed mount point to this test's private directory.
        self.assertEqual(BLOCK.count('"/sandbox/agent"'), 1)
        block = BLOCK.replace('"/sandbox/agent"', shlex.quote(str(self.agent)))
        harness = "set -e\nAS_SANDBOX=\n"
        if as_sandbox:
            harness += """
sandbox_user() {
  printf '%s\n' "$*" >> "$CALL_LOG"
  "$@"
}
AS_SANDBOX=sandbox_user
"""
        return subprocess.run(
            ["bash", "-c", harness + block + "\npwd\n" + suffix],
            cwd=cwd or self.agent,
            env={"PATH": os.environ["PATH"], "CALL_LOG": str(self.parent / "calls")},
            capture_output=True, text=True, timeout=5,
        )

    def test_empty_unwritable_workspace_is_recreated_and_cwd_rebound(self):
        self.agent.chmod(0o555)
        result = self.run_block(suffix="printf artifact > checklist.txt\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(self.agent))
        self.assertEqual((self.agent / "checklist.txt").read_text(), "artifact")
        self.assertEqual(self.agent.stat().st_uid, os.geteuid())
        self.assertTrue(os.access(self.agent, os.W_OK))

    def test_writable_existing_workspace_is_not_replaced(self):
        self.agent.chmod(0o700)
        (self.agent / "user-code.py").write_text("print('preserved')\n")
        before = self.agent.stat()
        result = self.run_block()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.agent.stat().st_ino, before.st_ino)
        self.assertEqual(self.agent.stat().st_mode, before.st_mode)
        self.assertEqual((self.agent / "user-code.py").read_text(), "print('preserved')\n")

    def test_populated_unwritable_workspace_is_not_removed_or_chmodded(self):
        (self.agent / "user-code.py").write_text("retained input\n")
        self.agent.chmod(0o555)
        before = self.agent.stat()
        result = self.run_block()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.agent.stat().st_ino, before.st_ino)
        self.assertEqual(self.agent.stat().st_mode, before.st_mode)
        self.assertEqual((self.agent / "user-code.py").read_text(), "retained input\n")

    def test_unwritable_parent_is_not_modified(self):
        self.agent.chmod(0o555)
        self.parent.chmod(0o555)
        self.addCleanup(self.parent.chmod, 0o700)
        before = self.agent.stat()
        result = self.run_block()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.agent.stat().st_ino, before.st_ino)
        self.assertEqual(self.agent.stat().st_mode, before.st_mode)

    def test_custom_working_directory_is_preserved(self):
        self.agent.chmod(0o555)
        custom = self.parent / "custom"
        custom.mkdir()
        before = self.agent.stat()
        result = self.run_block(cwd=custom)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(custom))
        self.assertEqual(self.agent.stat().st_ino, before.st_ino)
        self.assertEqual(self.agent.stat().st_mode, before.st_mode)

    def test_root_dev_path_uses_sandbox_identity_for_checks_and_mutations(self):
        self.agent.chmod(0o555)
        result = self.run_block(as_sandbox=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.parent / "calls").read_text().splitlines(), [
            "test -w .", "test -w ..", f"rmdir -- {self.agent}", "mkdir -- agent",
        ])

    def test_image_workdir_and_entrypoint_override_contract_remain_unchanged(self):
        dockerfile = (ROOT / "sandbox-images/hermes/Dockerfile").read_text()
        self.assertIn("WORKDIR /sandbox/agent", dockerfile)
        self.assertLess(ENTRYPOINT.index(BLOCK), ENTRYPOINT.index('mkdir -p "$HERMES_HOME"'))
        self.assertIn('exec $AS_SANDBOX "$@"', ENTRYPOINT)


if __name__ == "__main__":
    unittest.main()
