"use client";

// kars Bridge — compliance evidence pack. The mission's signed Governance
// Receipt expressed in the auditor's frameworks: each receipt claim is mapped to
// EU AI Act articles and NIST AI RMF functions, backed by the signed envelope
// digest + transparency-log inclusion. No competitor ships this from first-party
// audit data. Status is inherited verbatim from the signed claim — a PARTIAL
// control is never rendered as satisfied.
//
// `advisory` controls (the `regulatory` claim class) are a NAMED V0 product
// limitation — an external transparency anchor is a V1 roadmap item, so this
// claim reads PARTIAL on every receipt kars issues today, not a gap specific
// to this mission. Mirrors the same distinction AuditReceiptRow already makes
// when computing its "Verified" verdict (`isAdvisoryClaim`) — without this,
// the pack's own satisfied/partial count contradicted the receipt row sitting
// right above it.

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
  const advisoryCount = pack.advisory ?? advisoryControls.length;

  function download() {
    const blob = new Blob([JSON.stringify(pack, null, 2)], { type: "application/json" });
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
            This mission&apos;s signed receipt mapped to EU AI Act &amp; NIST AI RMF controls —
            first-party audit evidence, generated from the attestation, not a self-assessment.
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
        <span className="rounded-full border border-ok/40 bg-ok/10 px-2 py-0.5 text-ok">{pack.satisfied} satisfied</span>
        <span className="rounded-full border border-warning/40 bg-warning/10 px-2 py-0.5 text-warning">{pack.partial} partial</span>
        {advisoryCount > 0 && (
          <span
            className="rounded-full border border-border bg-surface-muted px-2 py-0.5 text-foreground-muted"
            title="Named V0 product limitation, not a gap in this mission — see below"
          >
            {advisoryCount} roadmap (V1)
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
                      {c.status.toUpperCase()}
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

      {/* Advisory (roadmap) controls — separated so they never blend into the
          "real gap" partial count above. Same claim, same framing the receipt
          row's own verdict already applies; the pack now agrees with it. */}
      {advisoryControls.length > 0 && (
        <div className="mt-4">
          <p className="flex items-center gap-1.5 text-xs font-semibold text-foreground-muted">
            <Icon name="compass" size={13} /> Roadmap — not yet available in this product version
          </p>
          <p className="mt-0.5 text-[11px] text-foreground-muted">
            These controls read PARTIAL on every receipt kars issues today — an external
            transparency anchor is a named V1 item, not a gap in this specific mission. They never
            block the mission&apos;s own &ldquo;Verified&rdquo; verdict.
          </p>
          <ul className="mt-1.5 divide-y divide-border overflow-hidden rounded-lg border border-dashed border-border">
            {advisoryControls.map((c) => (
              <li key={c.control_id} className="flex items-start gap-3 bg-surface-muted/30 px-3 py-2.5">
                <span className="mt-0.5 shrink-0 rounded-full border border-border bg-surface px-2 py-0.5 text-[10px] font-semibold text-foreground-muted">
                  ROADMAP
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
          Backed by envelope digest <span className="font-mono">{pack.envelope_digest.slice(0, 24)}…</span>,
          signature <span className="font-mono">{pack.signature_scheme}</span>
          {pack.inclusion_seq != null && <> , transparency-log entry #{pack.inclusion_seq}</>}.
        </p>
        <p className="mt-1">
          Verify independently: <span className="font-mono">{pack.verify_command}</span>
        </p>
      </div>
    </section>
  );
}
