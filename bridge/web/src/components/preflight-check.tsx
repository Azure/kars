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

import { useState, useTransition } from "react";
import { validatePackageAction } from "@/lib/preflight-actions";
import type { ValidationResult } from "@/lib/types";
import { ValidationChecks } from "./validation-checks";

export function PreflightCheck({
  blueprint,
  tier,
  budgetTokens,
  budgetScope,
  budgetUsdMicros,
  workload = "mission",
  onResult,
  disabled,
}: {
  blueprint: unknown;
  tier?: number;
  budgetTokens?: number | null;
  budgetScope?: "GovernedInference" | null;
  budgetUsdMicros?: number | null;
  workload?: "mission" | "team";
  /** Notified with the result so the caller can gate its launch/create button. */
  onResult?: (r: ValidationResult) => void;
  disabled?: boolean;
}) {
  const [pending, startTransition] = useTransition();
  const [result, setResult] = useState<ValidationResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  function run() {
    setError(null);
    startTransition(async () => {
      try {
        const r = await validatePackageAction(blueprint, {
          tier,
          budget_tokens: budgetTokens ?? null,
          budget_scope: budgetScope ?? null,
          budget_usd_micros: budgetUsdMicros ?? null,
          workload,
        });
        setResult(r);
        onResult?.(r);
      } catch (e) {
        setError(e instanceof Error ? e.message : "validation failed");
      }
    });
  }

  return (
    <section className="rounded-xl border border-border bg-surface p-5">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Pre-flight check</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            Validate the package against the live cluster before anything runs — the governance
            policy, requested model, connected services, network access, and any configured limits
            are checked for admissibility. It doesn&rsquo;t execute the mission or test the
            agent&rsquo;s behaviour.
          </p>
        </div>
        <button
          type="button"
          onClick={run}
          disabled={pending || disabled}
          className="shrink-0 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium transition hover:bg-surface-muted disabled:opacity-50"
        >
          {pending ? "Checking…" : "Validate package"}
        </button>
      </div>
      {error && <p role="alert" className="mt-2 text-xs text-danger">{error}</p>}
      {result && <ValidationChecks checks={result.checks} />}
      {result && !result.ok && (
        <p className="mt-2 text-xs font-medium text-danger">
          Fix the failing checks above before launching.
        </p>
      )}
    </section>
  );
}
