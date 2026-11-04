// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { TeamDetail } from "@/lib/types";

function exact(value: string): string {
  return BigInt(value).toLocaleString("en-US");
}

export function TeamBudgetStatus({ team }: { team: Pick<TeamDetail, "budget" | "budget_status"> }) {
  const status = team.budget_status;
  return (
    <section className="rounded-xl border border-border px-4 py-3" aria-label="Team lifetime inference budget">
      <h2 className="text-sm font-medium">Lifetime inference budget</h2>
      {status?.state === "available" ? (
        <div className="mt-2 space-y-2 text-sm">
          <p>
            Effective token ceiling: <strong>{status.unbounded_tokens
              ? "Unlimited · non-expiring token allowance"
              : status.effective_tokens !== null ? `${exact(status.effective_tokens)} tokens` : "No token ceiling on this account"}</strong>
          </p>
          <p>Effective monetary ceiling: {status.effective_usd_micros !== null
            ? `${exact(status.effective_usd_micros)} micro-USD (still enforced)`
            : "No monetary ceiling on this account"}</p>
          <p>Recorded account phase: {status.account_phase}. Observed at <time dateTime={status.observed_at}>{status.observed_at}</time>.</p>
          <dl className="grid gap-2 sm:grid-cols-3">
            {([
              ["Settled usage", status.settled],
              ["Reserved (not settled)", status.reserved],
              ["Uncertain charges", status.uncertain],
            ] as const).map(([label, amount]) => (
              <div key={label}>
                <dt className="text-xs text-foreground-muted">{label}</dt>
                <dd>{exact(amount.tokens)} tokens · {exact(amount.usd_micros)} micro-USD</dd>
              </div>
            ))}
          </dl>
          <p className="text-xs text-foreground-muted">{status.price_coverage_complete
            ? "Monetary meters use configured maximum prices, not an invoice."
            : "Price coverage is incomplete; actual monetary cost is unknown."}</p>
          <p className="text-xs text-foreground-muted">
            Shared by this Team’s principal, members and all runs. Recorded limits and charges are not a remaining balance
            or proof of live account readiness. Monetary caps and other admission, provider and authorization controls still apply.
          </p>
        </div>
      ) : (
        <p className="mt-2 text-sm" role="status">
          Effective budget and account usage unavailable. {status?.state === "unavailable"
            ? status.reason : "This response does not include a verified lifetime account observation."}
          {" "}Missing data is not zero usage or an unlimited allowance.
        </p>
      )}
      <details className="mt-3 text-xs text-foreground-muted">
        <summary className="cursor-pointer">Original lifetime inference grant (historical)</summary>
        {team.budget ? (
          <p className="mt-1">
            Scope: {team.budget.scope ?? "unscoped (not governed enrollment)"} · Tokens: {team.budget.tokens ?? "not configured"}
            {" · Micro-USD: "}{team.budget.usd_micros ?? "not configured"}.
            This immutable original envelope excludes later authorized top-ups and unbounded allowances; it is not the effective ceiling.
          </p>
        ) : <p className="mt-1">No explicit lifetime inference limit is configured in the original envelope. Tier defaults are not an unlimited allowance or aggregate spending protection.</p>}
      </details>
    </section>
  );
}
