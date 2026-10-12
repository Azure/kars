# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

from contextlib import closing
import hashlib
import importlib.util
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
DIRECTORY = ROOT / "sandbox-images/openclaw"
spec = importlib.util.spec_from_file_location("plugin_state", DIRECTORY / "plugin-state.py")
plugin_state = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plugin_state)


class PluginStateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.seed = self.root / "seed.sqlite"
        with closing(sqlite3.connect(self.seed)) as db:
            db.execute("CREATE TABLE records (name TEXT PRIMARY KEY, value TEXT)")
            db.execute("INSERT INTO records VALUES ('installed', 'genuine-fixture')")
            db.commit()
        self.seed.chmod(0o444)
        self.digest = hashlib.sha256(self.seed.read_bytes()).hexdigest()
        self.state = self.root / "sandbox/.openclaw"

    def initialize(self):
        plugin_state.initialize(self.seed, self.state)
        self.assertEqual(hashlib.sha256(self.seed.read_bytes()).hexdigest(), self.digest)
        self.assertFalse(Path(str(self.seed) + "-wal").exists())
        self.assertFalse(Path(str(self.seed) + "-shm").exists())

    def test_two_fresh_states_use_independent_writable_copies(self):
        for name in ("one", "two"):
            self.state = self.root / name
            self.initialize()
            db_path = self.state / "state/openclaw.sqlite"
            self.assertEqual(db_path.stat().st_mode & 0o777, 0o600)
            self.assertNotEqual(db_path.stat().st_ino, self.seed.stat().st_ino)
            with closing(sqlite3.connect(db_path)) as db:
                self.assertEqual(db.execute("SELECT * FROM records").fetchall(),
                                 [("installed", "genuine-fixture")])
                self.assertEqual(db.execute("PRAGMA integrity_check").fetchone(), ("ok",))

    def test_restart_preserves_live_database_including_wal(self):
        self.initialize()
        with closing(sqlite3.connect(self.state / "state/openclaw.sqlite")) as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("INSERT INTO records VALUES ('mission', 'retained')")
            db.commit()
            self.initialize()
            self.assertEqual(db.execute("SELECT value FROM records WHERE name='mission'").fetchone(),
                             ("retained",))

    def test_missing_or_changed_binding_does_not_replace_existing_data(self):
        for marker_value in (None, "another-image"):
            with self.subTest(marker=marker_value):
                self.state = self.root / str(marker_value)
                self.initialize()
                database = self.state / "state/openclaw.sqlite"
                before = database.read_bytes()
                marker = self.state / "state/kars-plugin-seed.sha256"
                if marker_value is None:
                    marker.unlink()
                else:
                    marker.write_text(marker_value)
                with self.assertRaisesRegex(RuntimeError, "Existing data was not replaced"):
                    self.initialize()
                self.assertEqual(database.read_bytes(), before)

    def test_interrupted_initialization_with_binding_only_can_resume(self):
        directory = self.state / "state"
        directory.mkdir(parents=True)
        (directory / "kars-plugin-seed.sha256").write_text(self.digest + "\n")
        self.initialize()
        self.assertTrue((directory / "openclaw.sqlite").is_file())

    def test_corrupted_seed_fails_without_publishing_database_or_marker(self):
        self.seed.chmod(0o600)
        self.seed.write_bytes(b"invalid sqlite input")
        with self.assertRaises(sqlite3.DatabaseError):
            self.initialize()
        self.assertEqual(list((self.state / "state").iterdir()), [])

    def test_missing_seed_fails_before_state_creation(self):
        self.seed.unlink()
        with self.assertRaises(FileNotFoundError):
            self.initialize()
        self.assertFalse(self.state.exists())

    def test_symlinks_are_not_followed(self):
        for kind in ("root", "state", "database", "marker", "lock"):
            with self.subTest(kind=kind):
                self.state = self.root / kind
                outside = self.root / (kind + "-outside")
                outside.mkdir()
                if kind == "root":
                    self.state.symlink_to(outside, target_is_directory=True)
                else:
                    self.state.mkdir()
                    if kind == "state":
                        (self.state / "state").symlink_to(outside, target_is_directory=True)
                    else:
                        (self.state / "state").mkdir()
                        path = {"database": "state/openclaw.sqlite",
                                "marker": "state/kars-plugin-seed.sha256",
                                "lock": ".kars-plugin-seed.lock"}[kind]
                        (self.state / path).symlink_to(outside / "untouched")
                with self.assertRaises((RuntimeError, OSError)):
                    self.initialize()
                self.assertEqual(list(outside.iterdir()), [])

    def test_concurrent_initializers_serialize_without_overwriting(self):
        code = ("import runpy,sys; from pathlib import Path; "
                "m=runpy.run_path(sys.argv[1]); "
                "m['initialize'](Path(sys.argv[2]),Path(sys.argv[3]))")
        commands = [sys.executable, "-B", "-c", code, str(DIRECTORY / "plugin-state.py"),
                    str(self.seed), str(self.state)]
        children = [subprocess.Popen(commands, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    for _ in range(3)]
        for child in children:
            out, err = child.communicate(timeout=15)
            self.assertEqual(child.returncode, 0, (out, err))
        self.initialize()

    def test_build_and_entrypoint_wiring_match_manifest(self):
        manifest = json.loads((DIRECTORY / "external-plugins.json").read_text())
        self.assertEqual({p["id"] for p in manifest["plugins"]},
                         {"brave", "tavily", "exa", "firecrawl", "perplexity",
                          "discord", "slack", "whatsapp"})
        self.assertEqual(len(manifest["plugins"]), 8)
        for plugin in manifest["plugins"]:
            self.assertRegex(plugin["integrity"], r"^sha512-[A-Za-z0-9+/]{86}==$")
        recipe = (DIRECTORY / "Dockerfile.base").read_text()
        self.assertIn("ARG OPENCLAW_VERSION=" + manifest["openclawVersion"], recipe)
        self.assertIn("python3 /opt/kars-build/plugin-state.py build", recipe)
        self.assertIn("COPY --from=builder /opt/openclaw-managed /opt/openclaw-managed", recipe)
        self.assertIn("chmod -R a-w /opt/openclaw-managed", recipe)
        entrypoint = (DIRECTORY / "entrypoint.sh").read_text()
        command = '$AS_SANDBOX python3 /usr/local/lib/kars-plugin-state.py initialize "$OPENCLAW_DIR"'
        self.assertIn(command, entrypoint)
        self.assertNotIn("|| true", entrypoint.split(command, 1)[1].splitlines()[0])


if __name__ == "__main__":
    unittest.main()
