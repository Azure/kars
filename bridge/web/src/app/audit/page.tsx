// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge — Auditor surface home. The read-only tamper-evident record and
// independent verification, rendered from the shared AuditView (identical to the
// Console's audit page, minus every operator write-control).

import { PageHeader } from "@/components/ui";
import { AuditView } from "@/components/audit-view";
import { getAudit } from "@/lib/bff";
import type { Audit } from "@/lib/types";

export const dynamic = "force-dynamic";

export default async function AuditorHome() {
  let audit: Audit | null = null;
  let error: unknown = null;
  try {
    audit = await getAudit();
  } catch (caught) {
    error = caught;
  }

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Auditor"
        title="Audit evidence"
        lead="Inspect recorded governance claims, log integrity, and per-receipt verification separately. Missing evidence is not proof of enforcement. This surface is read-only — nothing here can change the platform."
      />
      <AuditView audit={audit} error={error} />
    </div>
  );
}
