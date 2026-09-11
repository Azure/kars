# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from pathlib import Path
import subprocess
import tempfile
import unittest


class GitFixture(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="kars-ci-git-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Gate Fixture")
        self.git("config", "user.email", "gate@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "core.hooksPath", str(self.root / "empty-hooks"))
        self.git("commit", "--allow-empty", "-qm", "base")
        self.base = self.git("rev-parse", "HEAD").strip()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True,
                                       stderr=subprocess.PIPE)

    def write(self, name, text):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
