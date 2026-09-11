"""Qualification follows one immutable monorepo revision, never a stale core pin."""

import os
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from source_revision import checked_revision


class SourceRevisionTests(unittest.TestCase):
    def test_exact_workflow_revision_matches_the_real_checkout(self):
        revision = "a" * 40
        with patch.dict(os.environ, {"CORE_REVISION": revision}), \
                patch("source_revision.subprocess.run", return_value=SimpleNamespace(
                    returncode=0, stdout=revision + "\n")):
            self.assertEqual(checked_revision(), revision)

    def test_stale_missing_or_malformed_checkout_is_never_accepted(self):
        for code, actual, expected in (
            (1, "", "a" * 40), (0, "not-a-commit", "a" * 40),
            (0, "a" * 40, "b" * 40),
        ):
            with self.subTest(actual=actual), patch.dict(os.environ, {"CORE_REVISION": expected}), \
                    patch("source_revision.subprocess.run", return_value=SimpleNamespace(
                        returncode=code, stdout=actual)), self.assertRaises(RuntimeError):
                checked_revision()
