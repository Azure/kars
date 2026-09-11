# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

import os
from pathlib import Path
import subprocess
import unittest

from git_fixture import GitFixture

GATE = Path(__file__).resolve().parents[1] / "security-audit-required.sh"
OLD = "docs/security-audits/2026-01-01-old-scope.md"
NEW = "docs/security-audits/2026-01-02-current-scope.md"
SIGNED = ("# Approved old scope\n\n"
          "Signed-off-by: Author <author@example.invalid>\n"
          "Signed-off-by: Reviewer <reviewer@example.invalid>\n")


class SecurityAuditGateTests(GitFixture):
    def old_approval(self):
        self.write(OLD, SIGNED)
        self.commit()
        self.base = self.git("rev-parse", "HEAD").strip()

    def capability(self):
        self.write("cli/src/commands/capability.ts", "export const capability = true;\n")

    def gate(self):
        return subprocess.run(["bash", str(GATE)], cwd=self.root, text=True, capture_output=True,
                              env={**os.environ, "BASE_REF": self.base}, timeout=30)

    def test_modifying_an_old_signed_scope_does_not_approve_new_capability(self):
        self.old_approval()
        self.capability()
        self.write(OLD, SIGNED + "\nNew unreviewed implementation notes.\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("no new docs/security-audits/", result.stderr)

    def test_renaming_an_old_approval_is_not_a_new_review_record(self):
        self.old_approval()
        self.capability()
        self.git("mv", OLD, NEW)
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1)

    def test_new_unsigned_record_is_not_covered_by_old_signatures(self):
        self.old_approval()
        self.capability()
        self.write(OLD, SIGNED + "\nAdditional historical notes.\n")
        self.write(NEW, "# Current source review pending\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn(NEW, result.stderr)

    def test_new_record_still_requires_two_distinct_signers(self):
        self.capability()
        self.write(NEW, "# Current scope\nSigned-off-by: Author <same@example.invalid>\n"
                        "Signed-off-by: Reviewer <same@example.invalid>\n")
        self.commit()
        self.assertEqual(self.gate().returncode, 1)
        self.write(NEW, SIGNED.replace("Approved old scope", "Current scope"))
        self.commit()
        self.assertEqual(self.gate().returncode, 0)

    def test_documentation_only_changes_do_not_require_capability_approval(self):
        self.old_approval()
        self.write(OLD, SIGNED + "\nTypographic clarification.\n")
        self.commit()
        self.assertEqual(self.gate().returncode, 0)

    def test_missing_review_base_cannot_fall_back_to_an_empty_worktree_diff(self):
        self.capability()
        self.commit()
        self.base = "missing-reviewed-base"
        result = self.gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("cannot determine the reviewed capability diff", result.stderr)


if __name__ == "__main__":
    unittest.main()
