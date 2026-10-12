// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { ValidationResult } from "@/lib/types";

type Checks = ValidationResult["checks"];

function CheckList({ checks }: { checks: Checks }) {
  return (
    <ul className="space-y-3">
      {checks.map((check) => (
        <li key={check.id} className="min-w-0 text-sm">
          <p className="break-words font-medium">{check.label}</p>
          <p className="mt-1 break-words text-xs leading-relaxed text-foreground-muted">
            {check.detail}
          </p>
        </li>
      ))}
    </ul>
  );
}

export function ValidationChecks({ checks }: { checks: Checks }) {
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
          <CheckList checks={failures} />
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
