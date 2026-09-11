// kars Bridge Operator Console — Audit. The auditor's working surface, rendered
// from the shared AuditView so the Console and the dedicated /audit surface
// never drift. A chain-integrity verdict, then every Governance Receipt as an
// inspectable, independently-verifiable unit.

import { PageHeader } from "@/components/ui";
import { AuditView } from "@/components/audit-view";
import { getAudit } from "@/lib/bff";
import type { Audit } from "@/lib/types";

export const dynamic = "force-dynamic";

export default async function AuditPage() {
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
        eyebrow="Operator Console"
        title="Auditor view"
        lead="The tamper-evident record of everything the platform ran. Confirm the log is intact, inspect what each receipt attests, and verify any of it independently — no trust in this screen required."
      />
      <AuditView audit={audit} error={error} />
    </div>
  );
}
