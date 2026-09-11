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
  let error = false;
  try {
    audit = await getAudit();
  } catch {
    error = true;
  }

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Auditor"
        title="Tamper-evident record"
        lead="Everything the platform ran, as an independently-verifiable log. Confirm the chain is intact, inspect what each receipt attests, and verify any of it yourself. This surface is read-only — nothing here can change the platform."
      />
      <AuditView audit={audit} error={error} />
    </div>
  );
}
