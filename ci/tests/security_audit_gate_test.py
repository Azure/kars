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

    def gate(self, script=GATE, extra_env=None):
        return subprocess.run(["bash", str(script)], cwd=self.root, text=True, capture_output=True,
                              env={**os.environ, "BASE_REF": self.base,
                                   **(extra_env or {})}, timeout=30)

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

    def test_historical_header_changes_do_not_reopen_a_completed_scope(self):
        historical = "# Previously accepted scope\nSigned-off-by: Author <author@example.invalid>\n"
        self.write(OLD, historical)
        self.commit()
        self.base = self.git("rev-parse", "HEAD").strip()
        self.capability()
        self.write(OLD, "<!-- Copyright (c) Microsoft Corporation.\n"
                        "Licensed under the MIT License. -->\n\n" + historical)
        self.write(NEW, SIGNED.replace("Approved old scope", "Current scope"))
        self.commit()
        self.assertEqual(self.gate().returncode, 0)
        self.write(NEW, "# Current source review pending\n")
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn(NEW, result.stderr)

    def test_missing_review_base_cannot_fall_back_to_an_empty_worktree_diff(self):
        self.capability()
        self.commit()
        self.base = "missing-reviewed-base"
        result = self.gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("cannot determine the reviewed capability diff", result.stderr)

    audit = "docs/security-audits/2026-11-04-beta-publication.md"
    pr_number = "593"
    signer = "Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>"
    source_head = "28ff64372ca97e6bdd6db833d68afa90239cde7c"
    source_base = "b2ec2094cc663d5b747fec7f3c9d37cce667a24a"

    def approved_beta(self):
        self.git("commit", "--allow-empty", "-qm", "review base")
        self.base = self.git("rev-parse", "HEAD").strip()
        self.capability()
        self.write(self.audit, "# Unsigned beta candidate\n")
        self.commit()
        self.approved_head = self.git("rev-parse", "HEAD").strip()
        self.approval = ("# Explicit single-maintainer beta approval\n"
                         f"Beta-approval-exception: Azure/kars#{self.pr_number}; "
                         f"source={self.approved_head}; base={self.base}\n{self.signer}\n")
        self.write(self.audit, self.approval)
        self.commit()
        self.script = self.root / ".git" / "beta-gate-fixture.sh"
        self.script.write_text(GATE.read_text().replace(self.source_head, self.approved_head)
                               .replace(self.source_base, self.base))

    def beta_gate(self, **env):
        return self.gate(self.script, {"GITHUB_ACTIONS": "false", **env})

    def test_exact_beta_approval_accepts_one_real_signer(self):
        self.approved_beta()
        result = self.beta_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("single-maintainer beta exception", result.stdout)

    def test_policy_plumbing_does_not_change_the_approved_implementation(self):
        self.approved_beta()
        for name in ("ci/security-audit-required.sh", "ci/tests/security_audit_gate_test.py",
                     "docs/security-audits/README.md"):
            self.write(name, "Reviewed exception policy plumbing\n")
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 0)

    def assert_unapproved_change_rejected(self, name):
        self.approved_beta()
        self.write(name, "Not in the approved source\n")
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_implementation_change_invalidates_exception(self):
        self.assert_unapproved_change_rejected("cli/src/commands/capability.ts")

    def test_unrelated_document_change_invalidates_exception(self):
        self.assert_unapproved_change_rejected("README.md")

    def test_workflow_change_invalidates_exception(self):
        self.assert_unapproved_change_rejected(".github/workflows/ci-gates.yml")

    def test_unrelated_new_file_invalidates_exception(self):
        self.assert_unapproved_change_rejected("new-file.txt")

    def test_wrong_or_missing_approval_fields_fail_closed(self):
        self.approved_beta()
        for old, new in ((self.signer, ""), (self.signer, "Signed-off-by: Other <other@example.invalid>"),
                         (f"Azure/kars#{self.pr_number}", "Azure/kars#594"),
                         (self.approved_head, "0" * 40), (self.base, "1" * 40)):
            with self.subTest(field=old):
                self.write(self.audit, self.approval.replace(old, new))
                self.commit()
                self.assertEqual(self.beta_gate().returncode, 1)

    def test_dirty_uncommitted_approval_does_not_count(self):
        self.approved_beta()
        self.write(self.audit, "# Unsigned\n")
        self.commit()
        self.write(self.audit, self.approval)
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_another_new_unsigned_audit_still_fails(self):
        self.approved_beta()
        self.write(NEW, "# Another unsigned scope\n")
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_changed_base_does_not_extend_approval(self):
        self.approved_beta()
        self.base = self.git("rev-parse", f"{self.approved_head}^").strip() + "~1"
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_missing_approved_source_fails_closed(self):
        self.approved_beta()
        self.script.write_text(self.script.read_text().replace(self.approved_head, "0" * 40))
        self.write(self.audit, self.approval.replace(self.approved_head, "0" * 40))
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_wrong_ci_repository_event_or_pr_fails(self):
        self.approved_beta()
        context = {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": "Azure/kars",
                   "GITHUB_EVENT_NAME": "pull_request", "GITHUB_REF": f"refs/pull/{self.pr_number}/merge"}
        for key, value in (("GITHUB_REPOSITORY", "other/kars"),
                           ("GITHUB_REF", "refs/pull/594/merge"),
                           ("GITHUB_EVENT_NAME", "workflow_dispatch")):
            with self.subTest(field=key):
                self.assertEqual(self.beta_gate(**{**context, key: value}).returncode, 1)

    def test_synthetic_merge_and_push_first_parent_preserve_scope(self):
        self.approved_beta()
        candidate = self.git("rev-parse", "HEAD").strip()
        self.git("checkout", "-qb", "synthetic-merge", self.base)
        self.git("merge", "--no-ff", "--no-edit", candidate)
        self.assertEqual(self.beta_gate(GITHUB_ACTIONS="true", GITHUB_REPOSITORY="Azure/kars",
                                       GITHUB_EVENT_NAME="pull_request",
                                       GITHUB_REF=f"refs/pull/{self.pr_number}/merge").returncode, 0)
        self.assertEqual(self.beta_gate(BASE_REF="HEAD~1", GITHUB_ACTIONS="true",
                                       GITHUB_REPOSITORY="Azure/kars", GITHUB_EVENT_NAME="push",
                                       GITHUB_REF="refs/heads/kars-bridge").returncode, 0)

    def test_identical_tree_without_approved_ancestry_fails(self):
        self.approved_beta()
        tree = self.git("rev-parse", "HEAD^{tree}").strip()
        unrelated = self.git("commit-tree", tree, "-p", self.base,
                             "-m", "unreviewed copy of approved tree").strip()
        self.git("update-ref", "HEAD", unrelated)
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_exact_pr_and_unchanged_integration_contexts_pass(self):
        self.approved_beta()
        for event, ref in (("pull_request", f"refs/pull/{self.pr_number}/merge"),
                           ("push", "refs/heads/kars-bridge")):
            with self.subTest(event=event):
                result = self.beta_gate(GITHUB_ACTIONS="true", GITHUB_REPOSITORY="Azure/kars",
                                        GITHUB_EVENT_NAME=event, GITHUB_REF=ref)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_other_beta_record_cannot_be_changed_as_policy_plumbing(self):
        other = ("docs/security-audits/2026-11-10-overlay-package-update.md"
                 if self.pr_number == "593"
                 else "docs/security-audits/2026-11-04-beta-publication.md")
        self.assert_unapproved_change_rejected(other)

    def test_other_beta_pr_cannot_use_this_approval(self):
        self.approved_beta()
        other = "606" if self.pr_number == "593" else "593"
        result = self.beta_gate(GITHUB_ACTIONS="true", GITHUB_REPOSITORY="Azure/kars",
                                GITHUB_EVENT_NAME="pull_request",
                                GITHUB_REF=f"refs/pull/{other}/merge")
        self.assertEqual(result.returncode, 1)


class ReleaseRepairAuditGateTests(SecurityAuditGateTests):
    audit = "docs/security-audits/2026-11-10-overlay-package-update.md"
    pr_number = "606"
    source_head = "187ada4b45203bc831e7976961ffcb2f92160886"
    source_base = "ab78d11d66b626b4a176603643011ebc1cca1dff"


if __name__ == "__main__":
    unittest.main()
