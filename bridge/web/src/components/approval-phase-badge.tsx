// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge — approval phase badge.

export function approvalPhaseLabel(phase: string): string {
  return phase === "DecisionRecorded" ? "Decision recorded" : phase;
}

export function ApprovalPhaseBadge({ phase }: { phase: string }) {
  const map: Record<string, string> = {
    Pending: "border-warning/40 bg-warning/10 text-warning",
    DecisionRecorded: "border-warning/40 bg-warning/10 text-warning",
    Approved: "border-ok/40 bg-ok/10 text-ok",
    Denied: "border-danger/40 bg-danger/10 text-danger",
    Expired: "border-border bg-surface-muted text-foreground-muted",
    Stale: "border-border bg-surface-muted text-foreground-muted",
  };
  const cls = map[phase] ?? "border-border bg-surface-muted text-foreground-muted";
  return (
    <span
      title={phase === "DecisionRecorded" ? "Awaiting controller reconciliation; not a terminal outcome yet." : undefined}
      className={`inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-medium ${cls}`}
    >
      {approvalPhaseLabel(phase)}
    </span>
  );
}

const ACTION_LABELS: Record<string, string> = {
  toolCall: "Tool call",
  egress: "Egress",
  checkpoint: "Checkpoint",
  tierRaise: "Tier raise",
  budgetRaise: "Budget increase",
  clarification: "Clarification",
  irreversible: "Irreversible action",
  custom: "Action",
};

export function actionLabel(kind: string): string {
  return ACTION_LABELS[kind] ?? kind;
}
