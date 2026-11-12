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
        records = ("docs/security-audits/2026-11-04-beta-publication.md",
                   "docs/security-audits/2026-11-10-overlay-package-update.md",
                   "docs/security-audits/2026-11-10-kind-local-onboarding.md",
                   "docs/security-audits/2026-11-11-headlamp-go-release-repair.md",
                   "docs/security-audits/2026-11-12-openclaw-sdk-compare.md",
                   "docs/security-audits/2026-11-12-playwright-native-release.md",
                   "docs/security-audits/2026-11-12-openclaw-npm-registry.md",
                   "docs/security-audits/2026-11-12-headlamp-evidence-permissions.md")
        self.approved_beta()
        for other in records:
            if other != self.audit:
                with self.subTest(record=other):
                    self.assertFalse((self.root / other).exists())
                    self.write(other, "Not in the approved source\n")
                    self.commit()
                    self.assertEqual(self.beta_gate().returncode, 1)
                    self.git("rm", other)
                    self.commit()
                    self.assertEqual(self.beta_gate().returncode, 0)

    def test_other_beta_pr_cannot_use_this_approval(self):
        self.approved_beta()
        for other in ("593", "606", "607", "608", "617", "621", "622", "623"):
            if other != self.pr_number:
                with self.subTest(pr=other):
                    result = self.beta_gate(GITHUB_ACTIONS="true", GITHUB_REPOSITORY="Azure/kars",
                                            GITHUB_EVENT_NAME="pull_request",
                                            GITHUB_REF=f"refs/pull/{other}/merge")
                    self.assertEqual(result.returncode, 1)


class ReleaseRepairAuditGateTests(SecurityAuditGateTests):
    audit = "docs/security-audits/2026-11-10-overlay-package-update.md"
    pr_number = "606"
    source_head = "187ada4b45203bc831e7976961ffcb2f92160886"
    source_base = "ab78d11d66b626b4a176603643011ebc1cca1dff"


class LocalOnboardingAuditGateTests(SecurityAuditGateTests):
    audit = "docs/security-audits/2026-11-10-kind-local-onboarding.md"
    pr_number = "607"
    source_head = "a4333b355a03648cf3769faf4eca26e988700aeb"
    source_base = "97789571866ca2c181f84455a845b957427cd991"


class HeadlampReleaseAuditGateTests(SecurityAuditGateTests):
    audit = "docs/security-audits/2026-11-11-headlamp-go-release-repair.md"
    pr_number = "608"
    source_head = "bec8d459b360ee6c9d5416b313df8a356195bbd7"
    source_base = "2068be5faab76afeb1fa51afb7ebd20fd8177530"


class OpenClawSdkAuditGateTests(SecurityAuditGateTests):
    audit = "docs/security-audits/2026-11-12-openclaw-sdk-compare.md"
    pr_number = "617"
    source_head = "234b63e60f231f1c80286efaf9d2d44c91854f79"
    source_base = "e5db80c4e761cb5a92eb097b98713ba43d3d9394"


class PlaywrightNativeReleaseAuditGateTests(SecurityAuditGateTests):
    audit = "docs/security-audits/2026-11-12-playwright-native-release.md"
    pr_number = "621"
    source_head = "efadcd4932c08844a94e62080e45b6256c29a58a"
    source_base = "01548f2c365a94ca2016987f03a496a237ea46ff"

    def workflow_only(self):
        self.write(".github/workflows/release-public-interim.yml", "jobs: {}\n")
        self.write("cli/scripts/release-artifacts.test.mjs", "// Workflow contracts\n")

    def approved_workflow_beta(self):
        self.capability = self.workflow_only
        self.approved_beta()

    def test_workflow_only_source_checks_its_new_voluntary_approval(self):
        self.approved_workflow_beta()
        result = self.beta_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("single-maintainer beta exception", result.stdout)
        self.write(self.audit, self.approval.replace(self.approved_head, "0" * 40))
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_workflow_only_scope_rejects_followup_source_changes(self):
        self.approved_workflow_beta()
        self.write(".github/workflows/release-public-interim.yml", "jobs: {unapproved: {}}\n")
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_workflow_only_scope_rejects_other_pr_context(self):
        self.approved_workflow_beta()
        result = self.beta_gate(GITHUB_ACTIONS="true", GITHUB_REPOSITORY="Azure/kars",
                                GITHUB_EVENT_NAME="pull_request", GITHUB_REF="refs/pull/617/merge")
        self.assertEqual(result.returncode, 1)

    def test_historical_playwright_record_edit_does_not_reopen_approval(self):
        self.approved_workflow_beta()
        self.base = self.git("rev-parse", "HEAD").strip()
        self.write(self.audit, self.approval + "\nHistorical clarification.\n")
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 0)

    def test_unrelated_workflow_only_change_needs_no_capability_approval(self):
        self.workflow_only()
        self.commit()
        self.assertEqual(self.gate().returncode, 0)


class OpenClawNpmRegistryAuditGateTests(SecurityAuditGateTests):
    audit = "docs/security-audits/2026-11-12-openclaw-npm-registry.md"
    pr_number = "622"
    source_head = "28ca292c01bbd4310af90563fc2496281fce7141"
    source_base = "af5ff64796898060f8f9d4a9dc853013f1d51981"

    def capability(self):
        self.write("mesh-plugin/package-lock.json", '{"lockfileVersion": 3}\n')
        self.write("runtimes/openclaw/package-lock.json", '{"lockfileVersion": 3}\n')

    def assert_lock_requires_approval(self, path):
        self.write(path, '{"lockfileVersion": 3}\n')
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("no new docs/security-audits/", result.stderr)
        self.assertIn(path, result.stderr)

    def test_mesh_lock_requires_new_capability_approval(self):
        self.assert_lock_requires_approval("mesh-plugin/package-lock.json")

    def test_runtime_lock_requires_new_capability_approval(self):
        self.assert_lock_requires_approval("runtimes/openclaw/package-lock.json")

    def test_either_lock_change_invalidates_exact_source_approval(self):
        self.approved_beta()
        for path in ("mesh-plugin/package-lock.json", "runtimes/openclaw/package-lock.json"):
            with self.subTest(path=path):
                self.write(path, '{"lockfileVersion": 3, "unapproved": true}\n')
                self.commit()
                self.assertEqual(self.beta_gate().returncode, 1)
                self.write(path, '{"lockfileVersion": 3}\n')
                self.commit()
                self.assertEqual(self.beta_gate().returncode, 0)


class HeadlampEvidencePermissionsAuditGateTests(SecurityAuditGateTests):
    audit = "docs/security-audits/2026-11-12-headlamp-evidence-permissions.md"
    pr_number = "623"
    source_head = "15281ad70925895f3f7c646b6503a2b1f6bd90d1"
    source_base = "af5ff64796898060f8f9d4a9dc853013f1d51981"
    inherited_audit = "docs/security-audits/2026-11-12-openclaw-npm-registry.md"
    inherited_approval = (
        "# Published PR622 approval, not a PR623 sign-off\n"
        "Beta-approval-exception: Azure/kars#622; "
        "source=28ca292c01bbd4310af90563fc2496281fce7141; "
        "base=af5ff64796898060f8f9d4a9dc853013f1d51981\n"
        "Signed-off-by: Pal Lakatos-Toth <lakatos.toth.pal@gmail.com>\n"
    )

    def capability(self):
        self.write("sandbox-images/headlamp/Dockerfile", "FROM scratch\n")

    def approved_successor_beta(self):
        self.write(self.inherited_audit, self.inherited_approval)
        self.approved_beta()

    def test_headlamp_dockerfile_uses_ordinary_capability_gate(self):
        self.capability()
        self.commit()
        result = self.gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("no new docs/security-audits/", result.stderr)
        self.assertIn("sandbox-images/headlamp/Dockerfile", result.stderr)

    def test_document_only_record_has_no_workflow_style_exception(self):
        self.write(self.audit, "# Voluntary unsigned documentation\n")
        self.commit()
        self.assertEqual(self.gate().returncode, 0)

    def test_successor_carries_unchanged_prior_record_in_exact_contexts(self):
        self.approved_successor_beta()
        for event, ref in (("pull_request", "refs/pull/623/merge"),
                           ("push", "refs/heads/kars-bridge")):
            with self.subTest(event=event):
                result = self.beta_gate(GITHUB_ACTIONS="true", GITHUB_REPOSITORY="Azure/kars",
                                        GITHUB_EVENT_NAME=event, GITHUB_REF=ref)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"{self.inherited_audit} is unchanged", result.stdout)
                self.assertIn(f"{self.audit} uses its explicit", result.stdout)
        self.assertEqual((self.root / self.inherited_audit).read_text(), self.inherited_approval)

    def test_inherited_approval_does_not_authorize_any_prior_pr_context(self):
        self.approved_successor_beta()
        for other in ("593", "606", "607", "608", "617", "621", "622"):
            with self.subTest(pr=other):
                result = self.beta_gate(GITHUB_ACTIONS="true", GITHUB_REPOSITORY="Azure/kars",
                                        GITHUB_EVENT_NAME="pull_request",
                                        GITHUB_REF=f"refs/pull/{other}/merge")
                self.assertEqual(result.returncode, 1, result.stdout)

    def test_inherited_record_remains_immutable(self):
        self.approved_successor_beta()
        self.write(self.inherited_audit, self.inherited_approval + "\nRepointed historical scope.\n")
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_inherited_record_cannot_replace_missing_successor_record(self):
        self.approved_successor_beta()
        self.git("rm", self.audit)
        self.commit()
        result = self.beta_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn(self.inherited_audit, result.stderr)

    def test_inherited_record_cannot_hold_successor_marker(self):
        self.approved_successor_beta()
        self.git("rm", self.audit)
        self.write(self.inherited_audit, self.approval)
        self.commit()
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_inherited_record_cannot_bypass_successor_fields_or_signer(self):
        self.approved_successor_beta()
        for old, new in ((self.signer, ""), (self.signer, "Signed-off-by: Other <other@example.invalid>"),
                         ("Azure/kars#623", "Azure/kars#622"),
                         (self.approved_head, "0" * 40), (self.base, "1" * 40)):
            with self.subTest(field=old):
                self.write(self.audit, self.approval.replace(old, new))
                self.commit()
                self.assertEqual(self.beta_gate().returncode, 1)

    def test_two_signer_record_without_exact_marker_cannot_carry_prior_record(self):
        self.approved_successor_beta()
        self.write(self.audit, SIGNED)
        self.commit()
        result = self.beta_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn(self.inherited_audit, result.stderr)

    def test_inherited_record_does_not_hide_another_unsigned_scope(self):
        self.approved_successor_beta()
        self.write(NEW, "# Another unsigned scope\n")
        self.commit()
        result = self.beta_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn(NEW, result.stderr)

    def test_inherited_record_does_not_allow_implementation_or_lock_drift(self):
        self.approved_successor_beta()
        for path in ("sandbox-images/headlamp/Dockerfile", "mesh-plugin/package-lock.json",
                     "runtimes/openclaw/package-lock.json"):
            with self.subTest(path=path):
                original = (self.root / path).read_text() if (self.root / path).exists() else None
                self.write(path, "Unapproved implementation\n")
                self.commit()
                self.assertEqual(self.beta_gate().returncode, 1)
                if original is None:
                    self.git("rm", path)
                else:
                    self.write(path, original)
                self.commit()
                self.assertEqual(self.beta_gate().returncode, 0)

    def test_inherited_record_does_not_allow_base_drift(self):
        self.approved_successor_beta()
        self.base += "~1"
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_inherited_record_does_not_allow_source_ancestry_bypass(self):
        self.approved_successor_beta()
        tree = self.git("rev-parse", "HEAD^{tree}").strip()
        unrelated = self.git("commit-tree", tree, "-p", self.base,
                             "-m", "unreviewed successor tree").strip()
        self.git("update-ref", "HEAD", unrelated)
        self.assertEqual(self.beta_gate().returncode, 1)

    def test_successor_synthetic_merge_preserves_both_records(self):
        self.approved_successor_beta()
        candidate = self.git("rev-parse", "HEAD").strip()
        self.git("checkout", "-qb", "successor-merge", self.base)
        self.git("merge", "--no-ff", "--no-edit", candidate)
        result = self.beta_gate(GITHUB_ACTIONS="true", GITHUB_REPOSITORY="Azure/kars",
                                GITHUB_EVENT_NAME="pull_request", GITHUB_REF="refs/pull/623/merge")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.beta_gate(BASE_REF="HEAD~1", GITHUB_ACTIONS="true",
                                       GITHUB_REPOSITORY="Azure/kars", GITHUB_EVENT_NAME="push",
                                       GITHUB_REF="refs/heads/kars-bridge").returncode, 0)


if __name__ == "__main__":
    unittest.main()
