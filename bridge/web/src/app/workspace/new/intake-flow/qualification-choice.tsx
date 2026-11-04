// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export function QualificationChoice({ canQualify, checked, onChange }: {
  canQualify: boolean;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  if (!canQualify) return null;
  return (
    <div className="rounded-xl border border-warning/40 bg-warning/5 p-4">
      <label className="flex items-start gap-3 text-sm font-medium">
        <input type="checkbox" checked={checked} onChange={(event) => onChange(event.target.checked)}
          aria-describedby="qualification-help" className="mt-0.5 accent-signal" />
        Qualification run
      </label>
      <p id="qualification-help" className="mt-2 text-xs leading-relaxed text-foreground-muted">
        Test a configured route and selected catalogue resources that lack prior execution evidence.
        Missing evidence becomes a warning for this Mission only — resource readiness, security,
        finite budgets and content-bound skill approval still apply. This is not proof of qualification
        and does not apply to standing Teams. The reviewed package and current resource identities,
        specifications and versions remain binding. Admission must occur within
        45 minutes of create approval; this is not a runtime cancellation deadline.
      </p>
    </div>
  );
}
