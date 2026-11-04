<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Approvals and egress

Bridge presents governance requests in a shared inbox. Approval is a
resource transition, not a client-side button state.

In Inbox and Mission approvals, router-generated external-access requests show
where the work wants to connect, why it asked, and when the request expires.
The expiry is the review request's timestamp, not a promise about grant lifetime.
The original machine record is available under **Technical details**. Incomplete
records are identified rather than filled in with inferred permissions. This
presentation does not change whether a request is actionable or what approval
allows; Core still rechecks the request and its authority.

## Approval integrity

- Actor and roles come from the verified session.
- Decisions use resource-version compare-and-swap.
- Terminal decisions are immutable.
- Replays and stale generations return conflicts.
- Approval binds the current envelope or package digest.
- Self-approval is rejected where separation of duties applies.
- The Core CRD preserves requester subject/name and decision subject/roles,
  rather than letting Kubernetes prune the BFF's identity fields. Requester
  identity is immutable; legacy requests without identity keep their existing
  binding bytes.
- Approving a clarification requires a nonblank written answer, bounded to
  8192 UTF-8 bytes to match the router's response contract. Denying a Team
  milestone still requires written feedback; neither decision alone proves
  the agent received it or resumed.

After `spec.decision` is persisted, the API reports `DecisionRecorded` until
the controller supplies a terminal status. It is no longer actionable, but
does not yet mean the requested operation is approved or applied. The Inbox
keeps these records visible in a separate reconciliation section and refreshes
while visible. Returning to an idle Mission or Team tab also refreshes its
current state without restarting continuous polling.

## Egress modes

| Mode | Behavior |
|---|---|
| Learning | Records destinations and supports discovery; it is not equivalent to strict deny-all |
| Strict | Denies destinations not present in the signed baseline or an active approval |

These modes apply to standalone sandboxes. Governed Mission and standing-Team
tasks always use **Strict**: reviewed hosts and active approvals determine access.
Mission intake and Team creation, editing, and AI composition expose only Strict.
Their blueprint APIs reject Learning rather than accepting a mode that Core cannot
persist or execute. Omitting `egress_mode` retains Strict. Declared external hosts
remain editable during package review. The Mission network panel reports the
observed mode and baseline without a Learning toggle; unknown observations remain
unknown. New destinations during a run use the approval flow.

The older Mission `egress-mode` endpoint remains for existing clients updating
Strict baseline hosts (`strict` or `enforced`). Learning requests are rejected
before mutation; clearing hosts leaves Strict in place. The endpoint can be
removed when those older clients are retired. A saved baseline is not proof of
router reconciliation, and it excludes governed model/MCP access and temporary grants.

The router is the L7 enforcement point. Kubernetes NetworkPolicy and the
egress-guard contain the agent but do not replace host-level policy. Datapath
observations report sampled traffic; they neither grant access nor prove enforcement.

## Router clarification contract

The service-control-authenticated `POST /internal/access-requests/decision`
accepts `answer` only for an approved clarification. It requires the exact
`scope_id` and `request_id`; an agent admin credential is not sufficient.
The answer must be nonblank and at most 8192 UTF-8 bytes. The decision route
accepts a bounded 64 KiB JSON body to accommodate JSON escaping; the other
request routes keep their existing 8 KiB body limit.

The scoped agent wait/list responses retain the exact answer. A repeated
request cannot replace it, a later decision cannot overwrite it, and a scope
reset prevents the next assignment from retrieving it. Answers are not egress
or autonomy grants; `enforcement_changed` remains false.

## Required end-to-end flow

The following is the beta acceptance requirement, not evidence that the
installed path is complete. Router queuing and durable approval primitives
exist, but automatic queue-to-Inbox delivery and human-decision-to-agent
continuation still need integration and installed verification.

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
