// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
  let error: unknown = null;
  try {
    audit = await getAudit();
  } catch (caught) {
    error = caught;
  }

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="Auditor view"
        lead="Inspect recorded governance claims, log integrity, and per-receipt verification separately. Check the supporting evidence and trust-anchor limits before relying on a verdict."
      />
      <AuditView audit={audit} error={error} />
    </div>
  );
}
