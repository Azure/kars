# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import base64
import importlib.util
import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("task_filesystem", Path(__file__).with_name("task-filesystem.py"))
filesystem = importlib.util.module_from_spec(spec)
spec.loader.exec_module(filesystem)


class FilesystemTests(unittest.TestCase):
    def setUp(self):
        self.workspace = tempfile.TemporaryDirectory(prefix="kars-fs-", dir="/tmp")
        self.external = tempfile.TemporaryDirectory(prefix="kars-fs-outside-", dir="/var/tmp")
        self.addCleanup(self.workspace.cleanup)
        self.addCleanup(self.external.cleanup)
        self.root = Path(self.workspace.name)
        self.outside = Path(self.external.name)
        (self.outside / "document").write_bytes(b"external bytes must not change")

    def write(self, path, content=b"replacement bytes"):
        return filesystem.write_file(str(path), base64.b64encode(content).decode("ascii"))

    def read(self, path, limit=1024):
        return filesystem.read_file(str(path), limit)

    def test_nested_create_and_atomic_overwrite(self):
        target = self.root / "one" / "two" / "document"
        content = "Exact UTF-8\r\n\u2713".encode()
        result = self.write(target, content)
        self.assertEqual(result["bytes"], len(content))
        self.assertEqual(target.read_bytes(), content)
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
        self.write(target, b"")
        self.assertEqual(target.read_bytes(), b"")
        self.assertEqual(list(target.parent.iterdir()), [target])

    def test_hard_link_write_replaces_only_requested_entry(self):
        target = self.root / "document"
        os.link(self.outside / "document", target)
        self.write(target)
        self.assertEqual(target.read_bytes(), b"replacement bytes")
        self.assertEqual((self.outside / "document").read_bytes(), b"external bytes must not change")
        self.assertNotEqual(target.stat().st_ino, (self.outside / "document").stat().st_ino)

    def test_parent_symlinks_rejected_for_read_and_write(self):
        (self.root / "alias").symlink_to(self.outside, target_is_directory=True)
        with self.assertRaises(OSError):
            self.read(self.root / "alias" / "document")
        with self.assertRaises(OSError):
            self.write(self.root / "alias" / "new" / "document")
        self.assertFalse((self.outside / "new").exists())
        self.assertEqual((self.outside / "document").read_bytes(), b"external bytes must not change")

    def test_final_symlinks_rejected_for_read_and_write(self):
        target = self.root / "document"
        target.symlink_to(self.outside / "document")
        with self.assertRaises(OSError):
            self.read(target)
        with self.assertRaises(ValueError):
            self.write(target)
        self.assertTrue(target.is_symlink())
        self.assertEqual((self.outside / "document").read_bytes(), b"external bytes must not change")

    def test_non_regular_files_rejected_without_blocking(self):
        fifo = self.root / "fifo"
        os.mkfifo(fifo)
        for target in (fifo, self.root):
            with self.subTest(target=target):
                with self.assertRaises(ValueError):
                    self.read(target)
                with self.assertRaises(ValueError):
                    self.write(target)

    def test_missing_file_is_not_created_by_read(self):
        with self.assertRaises(FileNotFoundError):
            self.read(self.root / "missing" / "document")
        self.assertEqual(list(self.root.iterdir()), [])

    def test_paths_and_read_limits_fail_closed(self):
        for path in ("relative", "/tmp", "/sandbox", "/tmp/../etc/test", str(self.outside / "document"), "/tmp/\0x", "/tmp/" + "x" * 4096):
            with self.subTest(path=path):
                with self.assertRaises(ValueError):
                    self.read(path)
                with self.assertRaises(ValueError):
                    self.write(path)
        for limit in (0, -1, 1.5, True, "3", None, filesystem.MAX_BYTES + 1):
            with self.subTest(limit=limit), self.assertRaises(ValueError):
                self.read(self.root / "document", limit)

    def test_read_handles_short_reads_without_zero_padding(self):
        target = self.root / "document"
        target.write_bytes(b"123456789")
        original = os.read
        with patch.object(os, "read", side_effect=lambda fd, count: original(fd, min(count, 2))):
            result = self.read(target, 7)
        self.assertEqual(result["returned_bytes"], 7)
        self.assertEqual(base64.b64decode(result["base64"]), b"1234567")
        self.assertTrue(result["truncated"])
        with patch.object(os, "read", side_effect=lambda fd, count: original(fd, min(count, 2))):
            result = self.read(target)
        self.assertFalse(result["truncated"])
        self.assertEqual(base64.b64decode(result["base64"]), b"123456789")

    def test_early_eof_reports_incomplete_read(self):
        target = self.root / "document"
        target.write_bytes(b"123456789")
        original = os.read
        calls = 0

        def early_eof(fd, count):
            nonlocal calls
            calls += 1
            return original(fd, 2) if calls == 1 else b""

        with patch.object(os, "read", side_effect=early_eof):
            result = self.read(target)
        self.assertEqual(result["bytes"], 9)
        self.assertEqual(result["returned_bytes"], 2)
        self.assertTrue(result["truncated"])
        self.assertEqual(base64.b64decode(result["base64"]), b"12")

    def test_write_handles_partial_writes(self):
        original = os.write
        with patch.object(os, "write", side_effect=lambda fd, data: original(fd, data[:2])):
            self.write(self.root / "document")
        self.assertEqual((self.root / "document").read_bytes(), b"replacement bytes")

    def test_failed_write_preserves_previous_file_and_removes_temporary(self):
        target = self.root / "document"
        target.write_bytes(b"previous bytes")
        original = os.write
        calls = 0

        def failing_write(fd, data):
            nonlocal calls
            calls += 1
            if calls > 1:
                raise OSError("injected write failure")
            return original(fd, data[:2])

        for effect in (failing_write, lambda fd, data: 0):
            with self.subTest(effect=effect), patch.object(os, "write", side_effect=effect):
                with self.assertRaises(OSError):
                    self.write(target)
            self.assertEqual(target.read_bytes(), b"previous bytes")
            self.assertEqual(list(self.root.iterdir()), [target])

    def test_failed_replace_preserves_previous_file_and_removes_temporary(self):
        target = self.root / "document"
        target.write_bytes(b"previous bytes")
        with patch.object(os, "replace", side_effect=OSError("injected replace failure")):
            with self.assertRaises(OSError):
                self.write(target)
        self.assertEqual(target.read_bytes(), b"previous bytes")
        self.assertEqual(list(self.root.iterdir()), [target])

    def test_parent_swap_before_open_is_rejected(self):
        parent = self.root / "parent"
        original = os.open
        for operation in (self.read, self.write):
            parent.mkdir()
            (parent / "document").write_bytes(b"local bytes")

            def swap(path, flags, *args, **kwargs):
                if path == "parent" and kwargs.get("dir_fd") is not None:
                    parent.rename(self.root / "held")
                    parent.symlink_to(self.outside, target_is_directory=True)
                return original(path, flags, *args, **kwargs)

            with patch.object(os, "open", side_effect=swap), self.assertRaises(OSError):
                operation(parent / "document")
            self.assertEqual((self.outside / "document").read_bytes(), b"external bytes must not change")
            parent.unlink()
            (self.root / "held" / "document").unlink()
            (self.root / "held").rmdir()

    def test_parent_swap_after_open_uses_pinned_directory(self):
        parent = self.root / "parent"
        original = os.open
        for operation in (self.read, self.write):
            parent.mkdir()
            (parent / "document").write_bytes(b"local bytes")

            def swap(path, flags, *args, **kwargs):
                fd = original(path, flags, *args, **kwargs)
                if path == "parent" and kwargs.get("dir_fd") is not None:
                    parent.rename(self.root / "held")
                    parent.symlink_to(self.outside, target_is_directory=True)
                return fd

            with patch.object(os, "open", side_effect=swap):
                result = operation(parent / "document")
            if "base64" in result:
                self.assertEqual(base64.b64decode(result["base64"]), b"local bytes")
            else:
                self.assertEqual((self.root / "held" / "document").read_bytes(), b"replacement bytes")
            self.assertEqual((self.outside / "document").read_bytes(), b"external bytes must not change")
            parent.unlink()
            (self.root / "held" / "document").unlink()
            (self.root / "held").rmdir()

    def test_final_swap_before_replace_cannot_overwrite_symlink_target(self):
        target = self.root / "document"
        target.write_bytes(b"previous bytes")
        original = os.replace

        def swap(*args, **kwargs):
            target.unlink()
            target.symlink_to(self.outside / "document")
            return original(*args, **kwargs)

        with patch.object(os, "replace", side_effect=swap):
            self.write(target)
        self.assertFalse(target.is_symlink())
        self.assertEqual(target.read_bytes(), b"replacement bytes")
        self.assertEqual((self.outside / "document").read_bytes(), b"external bytes must not change")

    def test_final_swap_before_read_is_rejected(self):
        target = self.root / "document"
        target.write_bytes(b"local bytes")
        original = os.open

        def swap(path, flags, *args, **kwargs):
            if path == "document" and kwargs.get("dir_fd") is not None:
                target.unlink()
                target.symlink_to(self.outside / "document")
            return original(path, flags, *args, **kwargs)

        with patch.object(os, "open", side_effect=swap), self.assertRaises(OSError):
            self.read(target)

    def test_final_swap_after_read_open_uses_pinned_file(self):
        target = self.root / "document"
        target.write_bytes(b"local bytes")
        original = os.open

        def swap(path, flags, *args, **kwargs):
            fd = original(path, flags, *args, **kwargs)
            if path == "document" and kwargs.get("dir_fd") is not None:
                target.unlink()
                target.symlink_to(self.outside / "document")
            return fd

        with patch.object(os, "open", side_effect=swap):
            result = self.read(target)
        self.assertEqual(base64.b64decode(result["base64"]), b"local bytes")

    def test_symlink_raced_into_new_parent_is_rejected(self):
        parent = self.root / "new-parent"
        original = os.mkdir

        def swap(path, *args, **kwargs):
            original(path, *args, **kwargs)
            if path == "new-parent":
                parent.rmdir()
                parent.symlink_to(self.outside, target_is_directory=True)

        with patch.object(os, "mkdir", side_effect=swap), self.assertRaises(OSError):
            self.write(parent / "document")
        self.assertEqual((self.outside / "document").read_bytes(), b"external bytes must not change")

    def test_oversized_write_is_rejected_before_directory_creation(self):
        with self.assertRaises(ValueError):
            self.write(self.root / "new-parent" / "document", b"x" * (filesystem.MAX_BYTES + 1))
        self.assertEqual(list(self.root.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
