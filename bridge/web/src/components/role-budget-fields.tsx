// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { changeRoleBudget, planBudgetRoles, validTokenLimit } from "@/lib/budget-review";

export function RoleBudgetFields({ draft, onChange, idPrefix }: {
  draft: string;
  onChange: (draft: string) => void;
  idPrefix: string;
}) {
  let plan: unknown;
  try { plan = JSON.parse(draft); } catch { return null; }
  const roles = planBudgetRoles(plan);
  if (roles.length === 0) return null;
  return (
    <fieldset className="mt-5 border-t border-border pt-4">
      <legend className="pr-2 text-sm font-medium">Limits for individual roles</legend>
      <p id={`${idPrefix}-help`} className="mb-3 text-sm leading-relaxed text-foreground-muted">
        Each role shares the total above. You can give a role a smaller limit; leaving it blank
        means it uses the shared limit. Clearing the total does not clear these separate limits.
      </p>
      <div className="space-y-3">
        {roles.map((role) => {
          const id = `${idPrefix}-${role.index}`;
          const invalid = !validTokenLimit(role.tokens);
          return (
            <div key={id} className="grid items-start gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(10rem,14rem)]">
              <label htmlFor={id} className="break-words text-sm sm:pt-2">{role.name}</label>
              <div>
                <input id={id} type="text" inputMode="numeric" pattern="[0-9]*"
                  value={role.tokens == null ? "" : String(role.tokens)} placeholder="Use shared limit"
                  onChange={(event) => onChange(changeRoleBudget(draft, role.index, event.target.value))}
                  aria-label={`${role.name} token limit`}
                  aria-invalid={invalid} aria-describedby={`${idPrefix}-help${invalid ? ` ${id}-error` : ""}`}
                  className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm tabular-nums" />
                {invalid && <p id={`${id}-error`} role="alert" className="mt-1 text-xs text-danger">
                  Enter a positive whole number, or leave blank to use the shared limit.
                </p>}
              </div>
            </div>
          );
        })}
      </div>
    </fieldset>
  );
}
