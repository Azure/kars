// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import Link from "@/components/navigation-link";
import type { MissionQualification } from "@/lib/types";

export function NewQualificationMission() {
  return (
    <p className="text-xs leading-relaxed text-foreground-muted">
      Qualification Missions cannot be replicated, revised or run again as normal Missions.
      For another attempt, <Link href="/workspace/new" className="font-medium text-signal underline">
        create a new Mission
      </Link> and have an operator or admin explicitly designate and review a new qualification run.
      An existing unfinished durable request can still be resumed or awaited after the admission
      deadline, subject to server checks. Retained output can still be approved.
    </p>
  );
}

export function MissionQualificationRecord({ record, compact = false }: {
  record: MissionQualification;
  compact?: boolean;
}) {
  return (
    <section aria-label="Qualification run" className="rounded-xl border border-warning/40 bg-warning/5 p-4">
      <span className="inline-flex rounded-full border border-warning/40 px-2.5 py-1 text-xs font-medium text-warning">
        Qualification run
      </span>
      <p className="mt-2 text-xs leading-relaxed text-foreground-muted">
        Operator-approved testing of a configured route and selected catalogue resources, not proof of qualification.
        Missing prior execution evidence is advisory for this Mission only. Resource readiness,
        security, finite budgets and content-bound skill approval remain enforced. Resource identities,
        specifications and versions are bound to this approval and rechecked before admission.
      </p>
      <p className="mt-2 text-xs leading-relaxed text-foreground-muted">
        Admission deadline: <time dateTime={record.expires_at}>{record.expires_at}</time> —
        45 minutes after create approval, not a runtime cancellation deadline.
        The server decides admission; expiry does not cancel admitted work.
      </p>
      {!compact && (
        <>
          <dl className="my-4 grid gap-3 sm:grid-cols-2">
            {([
              ["Approved by", record.approved_by],
              ["Approver subject", record.approved_by_sub],
              ["Create approval time", record.approved_at],
              ["Admission deadline", record.expires_at],
              ["Reviewed package SHA-256", record.package_sha256],
            ] as const).map(([label, value]) => (
              <div key={label} className="min-w-0">
                <dt className="text-xs text-foreground-muted">{label}</dt>
                <dd className="mt-0.5 break-all text-sm">{value}</dd>
              </div>
            ))}
          </dl>
          {Object.entries(record.resource_bindings ?? {}).length > 0 && (
            <dl className="my-4 space-y-3" aria-label="Reviewed resource bindings">
              {Object.entries(record.resource_bindings ?? {}).map(([name, digest]) => (
                <div key={name} className="min-w-0">
                  <dt className="text-xs text-foreground-muted">{name}</dt>
                  <dd className="mt-0.5 break-all text-sm">{digest}</dd>
                </div>
              ))}
            </dl>
          )}
          <NewQualificationMission />
        </>
      )}
    </section>
  );
}
