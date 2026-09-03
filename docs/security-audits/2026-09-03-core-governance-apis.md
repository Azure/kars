# Security Audit — Core governance APIs

Date: 2026-09-03
Scope: `controller/src/kars_task.rs`, `controller/src/kars_task_reconciler.rs`, `controller/src/kars_task_execution.rs`, `controller/src/kars_approval.rs`, `controller/src/kars_receipt.rs`, `controller/src/kars_receipt_log.rs`, `cli/src/commands/approval.ts`, `cli/src/commands/receipt.ts`.
Gated paths: `controller/src/crd_validations.rs`, `cli/src/commands/approval.ts`, `cli/src/commands/receipt.ts`.

## Summary

This slice introduces the minimal Kubernetes APIs for governed tasks, human
approval, signed receipts, and receipt-log checkpoints. The controller remains
the authority for execution materialization and status; CLI commands only
submit or verify typed resources.

## T1: New capability / attack surface? (YES)

- Adds namespaced `KarsTask`, `KarsApproval`, and `KarsReceipt` resources.
- Adds controller reconciliation for task-to-sandbox execution and approval
  binding.
- Adds CLI read/write surfaces for approvals and receipt verification.

## T2: Security-control change? (YES)

- Delegated task envelopes must attenuate tier, budget, policy, egress, and
  delegation depth relative to their parent.
- Receipts are DSSE/Ed25519 signed and linked through a checkpointed inclusion
  log.
- Approval requests are bound to a task envelope digest and guarded by CEL
  request-shape validation.
- Task deletion removes owned execution resources so stale sandboxes do not
  retain authority.

## T3: Availability / fail-open risk? (REDUCED)

- Invalid or amplified envelopes fail closed before sandbox materialization.
- Missing model bindings surface degraded task state rather than silently
  launching unusable agents.
- Receipt and approval failures remain visible in status and do not fabricate
  successful governance evidence.

## Verification

- Full Rust workspace tests and doctests.
- Controller, router, CLI, Helm, CNCF conformance, formatting, clippy, LOC,
  no-stubs, no-custom-crypto, null-provider, module-isolation, and copyright
  gates.
- CLI approval and receipt tests plus package build.

## Verdict

Accept. The new authority surfaces are typed, attenuating, controller-owned,
and covered by signed evidence plus fail-closed admission and reconciliation.

Signed-off-by: Pal Lakatos-Toth <pallakatos@github.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
