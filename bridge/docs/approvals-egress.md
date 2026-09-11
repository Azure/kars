# Approvals and egress

Bridge presents governance requests in a shared inbox. Approval is a
resource transition, not a client-side button state.

## Approval integrity

- Actor and roles come from the verified session.
- Decisions use resource-version compare-and-swap.
- Terminal decisions are immutable.
- Replays and stale generations return conflicts.
- Approval binds the current envelope or package digest.
- Self-approval is rejected where separation of duties applies.

## Egress modes

| Mode | Behavior |
|---|---|
| Learning | Records destinations and supports discovery; it is not equivalent to strict deny-all |
| Strict | Denies destinations not present in the signed baseline or an active approval |

The router is the L7 enforcement point. Kubernetes NetworkPolicy and the
egress-guard contain the agent but do not replace host-level policy.

## Request flow

1. A mission attempts an unapproved destination or explicitly requests access.
2. Kars records a pending `KarsApproval`.
3. The request includes destination, port, task, reason, and bounded authority.
4. An operator approves or denies through Bridge.
5. The controller materializes the `EgressApproval`.
6. The router reloads the active grant.
7. Expiry or revocation restores denial.

Approving one host must not allow adjacent domains or wildcard expansion.

## Verification

For a high-confidence test:

- prove the destination did not receive traffic before approval;
- approve one exact host;
- verify that host succeeds;
- verify an adjacent host remains denied;
- expire or revoke the grant and verify denial returns.
