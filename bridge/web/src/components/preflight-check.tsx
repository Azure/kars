// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

// kars Bridge — shared pre-flight check. The SAME honest, cluster-grounded
// validation for both missions and teams: it validates the composed package
// against the live cluster (tools, connected services, model, egress, budget,
// tier) before anything runs, and reports whether the package is launch-ready.
//
// Used by the mission intake and the team composer so the two flows are
// consolidated on one validation surface.

import { useEffect, useRef, useState, useTransition } from "react";
import { checkPackage } from "@/lib/preflight-client";
import type { ValidationResult } from "@/lib/types";
import { ValidationChecks } from "./validation-checks";

export function PreflightCheck({
  blueprint,
  teamRoles,
  selectedRepos,
  tier,
  delegationDepth,
  budgetTokens,
  budgetScope,
  budgetUsdMicros,
  workload = "mission",
  qualificationRun = false,
  onResult,
  onReviewBudget,
  disabled,
}: {
  blueprint: unknown;
  teamRoles?: import("@/lib/bff-contracts").CreateRole[];
  selectedRepos?: string[];
  tier?: number;
  delegationDepth?: number;
  budgetTokens?: number | null;
  budgetScope?: "GovernedInference" | null;
  budgetUsdMicros?: number | null;
  workload?: "mission" | "team";
  qualificationRun?: boolean;
  /** Notified with the result so the caller can gate its launch/create button. */
  onResult?: (r: ValidationResult) => void;
  onReviewBudget?: () => void;
  disabled?: boolean;
}) {
  const [pending, startTransition] = useTransition();
  const [result, setResult] = useState<ValidationResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => { live.current = false; };
  }, []);

  function run() {
    setError(null);
    setResult(null);
    onResult?.({ ok: false, checks: [] });
    startTransition(async () => {
      try {
        const r = await checkPackage(blueprint, {
          tier,
          ...(delegationDepth === undefined ? {} : { delegation_depth: delegationDepth }),
          budget_tokens: budgetTokens ?? null,
          budget_scope: budgetScope ?? null,
          budget_usd_micros: budgetUsdMicros ?? null,
          workload,
          ...(teamRoles === undefined ? {} : { team_roles: teamRoles }),
          ...(selectedRepos === undefined ? {} : { git_write_repos: selectedRepos }),
          qualification_run: qualificationRun || undefined,
        });
        if (!live.current) return;
        setResult(r);
        onResult?.(r);
      } catch (e) {
        if (!live.current) return;
        setError(e instanceof Error ? e.message : "validation failed");
      }
    });
  }

  return (
    <section className="rounded-xl border border-border bg-surface p-5">
      <div className="flex flex-col items-start justify-between gap-3 sm:flex-row">
        <div>
          <h2 className="text-sm font-semibold">Check before starting</h2>
          <p className="mt-1 max-w-prose text-sm leading-relaxed text-foreground-muted">
            Check that your chosen AI, connected services, permissions and spending settings
            are ready. This does not start the work or test the quality of its results.
          </p>
        </div>
        <button
          type="button"
          onClick={run}
          disabled={pending || disabled}
          className="shrink-0 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium transition hover:bg-surface-muted disabled:opacity-50"
        >
          {pending ? "Checking…" : result || error ? "Check again" : "Check readiness"}
        </button>
      </div>
      {error && <p role="alert" className="mt-2 text-xs text-danger">{error}</p>}
      {disabled && <p className="mt-2 text-sm text-foreground-muted">Correct the highlighted settings above before checking.</p>}
      {result && <ValidationChecks checks={result.checks}
        budget={{ blueprint, tokens: budgetTokens, usdMicros: budgetUsdMicros }} onReviewBudget={onReviewBudget} />}
      {result && !result.ok && (
        <p className="mt-2 text-xs font-medium text-danger">
          Fix the failing checks above before launching.
        </p>
      )}
    </section>
  );
}
