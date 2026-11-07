// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use client";

import { Icon } from "@/components/icon";
import type { CompliancePack } from "@/lib/types";

function statusTone(s: string): string {
  const u = s.toUpperCase();
  if (u === "PASS") return "border-ok/40 bg-ok/10 text-ok";
  if (u === "PARTIAL") return "border-warning/40 bg-warning/10 text-warning";
  return "border-danger/40 bg-danger/10 text-danger";
}

export function CompliancePackView({ pack }: { pack: CompliancePack }) {
  const coreControls = pack.controls.filter((c) => !c.advisory);
  const advisoryControls = pack.controls.filter((c) => c.advisory);
  const frameworks = [...new Set(coreControls.map((c) => c.framework))];
  const passingCount = coreControls.filter((c) => c.status.toUpperCase() === "PASS").length;
  const interpretation = "These mappings describe recorded claims, not control-enforcement proof or regulatory certification. Opening or downloading this pack does not run receipt verification. The legacy satisfied count counts mapped PASS claims, not satisfied obligations. Legacy aggregate fields are preserved unchanged; inspect controls for each recorded status. Multiple mappings may share one claim. Signature and inclusion metadata are recorded pointers, not verification results; a cluster-published key is not an independent trust anchor. Advisory statuses remain evidence limitations, not verified compliance.";

  function download() {
    const blob = new Blob([JSON.stringify({ ...pack, interpretation }, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `compliance-evidence-${pack.task}.json`;
    a.style.display = "none";
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  return (
    <section className="rounded-xl border border-border bg-surface p-5">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Compliance evidence pack</h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            Recorded receipt claims mapped to EU AI Act &amp; NIST AI RMF references —
            not control-enforcement proof or regulatory certification. Opening or downloading
            this pack does not run receipt verification.
          </p>
        </div>
        <button
          type="button"
          onClick={download}
          className="shrink-0 rounded-lg border border-border bg-surface-muted px-3 py-1.5 text-xs font-medium transition hover:bg-surface-muted/70"
          title="Download the evidence pack as JSON for an auditor"
        >
          <Icon name="download" size={12} className="inline mr-1" /> Download pack
        </button>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-2 text-xs">
        <span className="rounded-full border border-ok/40 bg-ok/10 px-2 py-0.5 text-ok">{passingCount} mapped passing claims</span>
        <span className="rounded-full border border-warning/40 bg-warning/10 px-2 py-0.5 text-warning">{coreControls.length - passingCount} other mapped claims</span>
        {advisoryControls.length > 0 && (
          <span
            className="rounded-full border border-border bg-surface-muted px-2 py-0.5 text-foreground-muted"
            title="Advisory claims are shown separately, with their recorded status"
          >
            {advisoryControls.length} advisory mappings
          </span>
        )}
        <span className="text-foreground-muted">across {frameworks.join(" · ")}</span>
      </div>

      <div className="mt-4 space-y-4">
        {frameworks.map((fw) => (
          <div key={fw}>
            <p className="text-xs font-semibold text-foreground-muted">{fw}</p>
            <ul className="mt-1.5 divide-y divide-border overflow-hidden rounded-lg border border-border">
              {coreControls
                .filter((c) => c.framework === fw)
                .map((c) => (
                  <li key={c.control_id} className="flex items-start gap-3 bg-surface px-3 py-2.5">
                    <span className={`mt-0.5 shrink-0 rounded-full border px-2 py-0.5 text-[10px] font-semibold ${statusTone(c.status)}`}>
                      Claim {c.status.toUpperCase()}
                    </span>
                    <div className="min-w-0">
                      <p className="text-sm font-medium">{c.reference}</p>
                      <p className="mt-0.5 text-xs text-foreground-muted">
                        Evidence (receipt claim &ldquo;{c.receipt_class}&rdquo;): {c.evidence}
                      </p>
                    </div>
                  </li>
                ))}
            </ul>
          </div>
        ))}
      </div>

      {advisoryControls.length > 0 && (
        <div className="mt-4">
          <p className="flex items-center gap-1.5 text-xs font-semibold text-foreground-muted">
            <Icon name="compass" size={13} /> Advisory claims and evidence limitations
          </p>
          <p className="mt-0.5 text-[11px] text-foreground-muted">
            Advisory claims are separate from the required receipt claims. Missing or omitted
            evidence remains a limitation even when cryptographic checks pass. Inspect the
            recorded status and supporting evidence; advisory does not mean satisfied.
          </p>
          <ul className="mt-1.5 divide-y divide-border overflow-hidden rounded-lg border border-dashed border-border">
            {advisoryControls.map((c) => (
              <li key={c.control_id} className="flex items-start gap-3 bg-surface-muted/30 px-3 py-2.5">
                <span className="mt-0.5 shrink-0 rounded-full border border-border bg-surface px-2 py-0.5 text-[10px] font-semibold text-foreground-muted">
                  Claim {c.status.toUpperCase()}
                </span>
                <div className="min-w-0">
                  <p className="text-sm font-medium text-foreground-muted">{c.reference}</p>
                  <p className="mt-0.5 text-xs text-foreground-muted">
                    Evidence (receipt claim &ldquo;{c.receipt_class}&rdquo;): {c.evidence}
                  </p>
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="mt-4 rounded-lg border border-border bg-surface-muted/40 p-3 text-[11px] text-foreground-muted">
        <p>
          Recorded envelope digest <span className="font-mono">{pack.envelope_digest.slice(0, 24)}…</span>,
          signature scheme <span className="font-mono">{pack.signature_scheme}</span>
          {pack.inclusion_seq != null && <> , inclusion-log entry #{pack.inclusion_seq}</>}.
          These pointers are not verification results. Multiple mappings may share one claim.
        </p>
        <p className="mt-1">
          Re-check the receipt: <span className="font-mono">{pack.verify_command}</span>.
          A cluster-published key is not an independent trust anchor.
        </p>
      </div>
    </section>
  );
}
