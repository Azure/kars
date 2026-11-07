// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { useState } from "react";
import type { ValidationResult } from "@/lib/types";
import { requestedBudgetLines, type BudgetReview } from "@/lib/budget-review";

type Checks = ValidationResult["checks"];

function BudgetContractHelp({ check, budget, onReviewBudget }: {
  check: Checks[number]; budget?: BudgetReview; onReviewBudget?: () => void;
}) {
  const [copyState, setCopyState] = useState<"idle" | "copied" | "manual">("idle");
  const limits = budget ? requestedBudgetLines(budget) : [];
  const supportRequest = [
    "Please help me resolve a Kars spending check. I want to keep my intended limits.",
    "The package did not pass validation. The check does not launch work.",
    ...limits, "", "Validation detail:", check.detail,
    "", "Please review the applicable model spending contracts, reservation capacity, and inherited limits. Connecting a provider does not enroll its spending contract.",
  ].join("\n");
  async function copy() {
    try { await navigator.clipboard.writeText(supportRequest); setCopyState("copied"); }
    catch { setCopyState("manual"); }
  }
  return (
    <div className="mt-2 space-y-4">
      <p className="max-w-prose text-sm leading-relaxed text-foreground-muted">
        Kars could not confirm that this work can run within its spending limits.
        This check has not started any work or changed your settings.
      </p>
      <div>
        <p className="text-sm font-medium">What to do next</p>
        <p className="mt-1 max-w-prose text-sm leading-relaxed text-foreground-muted">
          Review the limits you want. If they look right—or you are unsure—send a support request
          to the person who manages Kars for you. You do not need to reconnect your AI account.
        </p>
        <div className="mt-3 flex flex-wrap gap-2">
          {onReviewBudget && <button type="button" onClick={onReviewBudget}
            className="rounded-lg border border-border bg-surface px-3 py-2 text-sm font-medium hover:bg-surface-muted">
            Review spending limits
          </button>}
          <button type="button" onClick={copy}
            className="rounded-lg border border-border bg-surface px-3 py-2 text-sm font-medium hover:bg-surface-muted">
            Copy support request
          </button>
        </div>
        <p role="status" className="mt-2 text-xs text-foreground-muted">
          {copyState === "copied" ? "Copied. Paste this into a message to your Kars administrator; nothing has been sent automatically."
            : copyState === "manual" ? "Your browser could not copy the message. Select and copy the text below."
              : "The request includes this check and its model/role details. You choose who receives it."}
        </p>
        {copyState === "manual" && <textarea aria-label="Support request to copy" readOnly value={supportRequest}
          onFocus={(event) => event.currentTarget.select()} rows={8}
          className="mt-2 w-full rounded-lg border border-border bg-surface p-3 text-sm" />}
      </div>
      {limits.length > 0 && <div>
        <p className="text-sm font-medium">Limits checked</p>
        <ul className="mt-1 space-y-1 text-sm text-foreground-muted">
          {limits.map((line, index) => <li key={index} className="break-words">{line}</li>)}
        </ul>
        <p className="mt-2 max-w-prose text-xs leading-relaxed text-foreground-muted">
          Tokens measure text the AI reads and writes, not money. These are requested limits, not
          remaining balances. Workspace and profile limits may also apply. An unbounded workspace
          allowance does not remove a separate limit here.
        </p>
      </div>}
      <details className="border-t border-border pt-3">
        <summary className="cursor-pointer text-sm font-medium">Technical details for your administrator</summary>
        <div className="mt-3 max-w-prose space-y-3 text-sm leading-relaxed text-foreground-muted">
          <p className="break-words">{check.detail}</p>
          <p>
            A model spending contract is administrator-reviewed request bounds, not a legal agreement,
            provider login, or token allowance. Input, output and reasoning bounds support reservation;
            cost ceilings additionally require maximum prices. A blank shared token limit does not
            clear a finite role limit. Do not remove an intended limit to bypass this check.
          </p>
          <p>
            Confirm provider-supported bounds for the exact provider identity, endpoint, model and
            operation. Add or renew the reviewed route through Core Helm
            {" "}<code>inferenceBudget.contracts</code> with current validity and a new
            {" "}<code>inferenceBudget.catalogVersion</code>. Preserve existing contracts and funding.
            Follow <code>docs/governed-inference-budgets.md</code>; do not guess bounds or prices.
            There is currently no browser contract editor. Daily budgets on the Policies page are separate.
          </p>
          <p>
            Without effective token or cost ceilings, intentionally unbounded work needs no model
            spending contract. Accounting, funding authorization, provider limits and security checks
            still apply. After correction, check again; validation is not execution proof.
          </p>
        </div>
      </details>
    </div>
  );
}

export function CompositionNote({ detail }: { detail: string }) {
  if (!/\bNo current operator budget contract covers model\b/i.test(detail)) return <p>{detail}</p>;
  return <div>
    <p className="font-medium">Spending setup needs attention</p>
    <BudgetContractHelp key={detail} check={{ id: "budget_contracts", status: "fail", label: "Spending setup needs attention", detail }} />
  </div>;
}

type CheckProps = { checks: Checks; budget?: BudgetReview; onReviewBudget?: () => void };

function CheckList({ checks, budget, onReviewBudget }: CheckProps) {
  return (
    <ul className="space-y-3">
      {checks.map((check) => (
        <li key={check.id} className="min-w-0 text-sm">
          <p className="break-words font-medium">
            {check.id === "budget_contracts" && check.status === "fail" ? "Spending setup needs attention" : check.label}
          </p>
          {check.id === "budget_contracts" && check.status === "fail"
            ? <BudgetContractHelp key={JSON.stringify([check.detail, budget])} check={check} budget={budget} onReviewBudget={onReviewBudget} />
            : <p className="mt-1 break-words text-xs leading-relaxed text-foreground-muted">{check.detail}</p>}
        </li>
      ))}
    </ul>
  );
}

export function ValidationChecks({ checks, budget, onReviewBudget }: CheckProps) {
  const failures = checks.filter((check) => check.status === "fail");
  const warnings = checks.filter((check) => check.status === "warn");
  const passed = checks.filter((check) => check.status === "pass");

  return (
    <div className="mt-3 space-y-3">
      {failures.length > 0 && (
        <div className="rounded-lg border border-danger/30 bg-danger/5 p-4">
          <h3 className="mb-3 text-sm font-semibold text-danger">
            {failures.length} {failures.length === 1 ? "check needs" : "checks need"} attention
          </h3>
          <CheckList checks={failures} budget={budget} onReviewBudget={onReviewBudget} />
        </div>
      )}
      {warnings.length > 0 && (
        <details className="rounded-lg border border-warning/30 bg-surface">
          <summary className="cursor-pointer rounded-lg p-3 text-sm font-medium text-warning transition hover:bg-surface-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal">
            {warnings.length} {warnings.length === 1 ? "warning" : "warnings"} to review
          </summary>
          <div className="border-t border-border px-4 py-3">
            <CheckList checks={warnings} />
          </div>
        </details>
      )}
      {passed.length > 0 && (
        <details className="rounded-lg border border-border bg-surface">
          <summary className="cursor-pointer rounded-lg p-3 text-sm font-medium transition hover:bg-surface-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal">
            {passed.length} {passed.length === 1 ? "check" : "checks"} passed
          </summary>
          <div className="border-t border-border px-4 py-3">
            <CheckList checks={passed} />
          </div>
        </details>
      )}
    </div>
  );
}
