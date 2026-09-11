"use client";

// kars Bridge Operator Console — a single auditable receipt row. Collapsed it
// shows the mission + signed verdict; expanded it shows exactly what the receipt
// attests (the claim classes with PASS/PARTIAL/FAIL), the signing scheme, the
// inclusion-log position, and the precise command an auditor runs to verify it
// independently. This is the auditor's working unit — not an opaque hash.

import { useState } from "react";
import type { Receipt, VerifyResult, CompliancePack } from "@/lib/types";
import { CompliancePackView } from "@/components/compliance-pack";

function claimTone(status: string): { cls: string; label: string } {
  const s = status.toUpperCase();
  if (s === "PASS" || s === "OK") return { cls: "border-emerald-500/30 bg-emerald-500/10 text-emerald-600", label: "PASS" };
  if (s === "PARTIAL") return { cls: "border-amber-500/30 bg-amber-500/10 text-amber-600", label: "PARTIAL" };
  if (s === "FAIL" || s === "ERROR") return { cls: "border-rose-500/30 bg-rose-500/10 text-rose-600", label: "FAIL" };
  return { cls: "border-border bg-surface-muted text-foreground-muted", label: status };
}

const CLAIM_LABEL: Record<string, string> = {
  integrity: "Integrity — is it authentic & unaltered?",
  conformance: "Conformance — did it stay within its authority?",
  completeness: "Completeness — were all controls enforced & recorded?",
  regulatory: "Regulatory — how is it anchored & signed?",
};

export function AuditReceiptRow({
  ns,
  task,
  summarySeq,
  summaryTask,
  summaryVerdict,
  summaryCreated,
}: {
  ns: string;
  task: string;
  summarySeq: number | null;
  summaryTask: string;
  summaryVerdict: "verified" | "failed" | "partial" | "none";
  summaryCreated?: string | null;
}) {
  const [open, setOpen] = useState(false);
  const [receipt, setReceipt] = useState<Receipt | null>(null);
  const [compliance, setCompliance] = useState<CompliancePack | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [verifying, setVerifying] = useState(false);
  const [result, setResult] = useState<VerifyResult | null>(null);
  const [verifyError, setVerifyError] = useState<string | null>(null);

  // Lazy-load the full receipt (claims + signature material) only when the row
  // is first expanded — so the audit page doesn't N+1-fetch every receipt into a
  // huge initial payload. Uses the same-origin /api proxy (never the server-only
  // @/lib/bff client, which would bundle server config into the browser).
  async function toggle() {
    const next = !open;
    setOpen(next);
    if (next && !receipt && !loading) {
      setLoading(true);
      setLoadError(null);
      try {
        const res = await fetch(
          `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/receipt`,
          { headers: { accept: "application/json" } },
        );
        if (res.status === 404) {
          setReceipt(null);
        } else if (!res.ok) {
          throw new Error(`load failed: ${res.status}`);
        } else {
          setReceipt(await res.json());
        }
        // Also pull the compliance evidence pack (EU AI Act / NIST AI RMF)
        // derived from this same signed receipt — so the auditor sees the
        // regulatory conformance mapping without leaving the auditor surface.
        try {
          const cres = await fetch(
            `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/compliance`,
            { headers: { accept: "application/json" } },
          );
          if (cres.ok) setCompliance(await cres.json());
        } catch {
          /* compliance is best-effort; the receipt still renders */
        }
      } catch {
        setLoadError("This receipt's full attestation couldn't be loaded.");
      } finally {
        setLoading(false);
      }
    }
  }

  async function runVerify() {
    if (!receipt) return;
    setVerifying(true);
    setVerifyError(null);
    setResult(null);
    try {
      const res = await fetch(
        `/api/namespaces/${encodeURIComponent(receipt.namespace)}/tasks/${encodeURIComponent(receipt.task)}/receipt/verify`,
        { method: "POST", headers: { accept: "application/json" } },
      );
      if (!res.ok) throw new Error(`verify failed: ${res.status}`);
      const r: VerifyResult = await res.json();
      setResult(r);
    } catch {
      setVerifyError("Verification couldn't be completed — the audit backend may be unreachable.");
    } finally {
      setVerifying(false);
    }
  }

  // Overall verdict: once the full receipt is loaded, derive it from its claim
  // set; before that, use the cheap summary verdict the BFF computed from the
  // receipt's claim matrix — so the collapsed row is accurate without an eager
  // full-receipt fetch.
  const claims = receipt?.claims ?? [];
  // Mirror the BFF verdict rule: the regulatory claim is a V0 maturity dimension
  // (always PARTIAL/OMITTED until an external anchor lands — a named V1 item),
  // and OMITTED is an honest disclosure, not a failure. Neither blocks a Verified
  // verdict, so the badge reflects the cryptographic claims (integrity +
  // conformance + completeness); the regulatory/omitted maturity is still shown
  // in the expanded claim detail. Without this, every receipt read "Partial".
  const isAdvisoryClaim = (c: { class?: string; status: string }) =>
    (c.class ?? "").toLowerCase() === "regulatory" || c.status.toUpperCase() === "OMITTED";
  const allStatuses = claims.map((c) => c.status.toUpperCase());
  const coreStatuses = claims.filter((c) => !isAdvisoryClaim(c)).map((c) => c.status.toUpperCase());
  const effectiveVerdict = receipt
    ? allStatuses.includes("FAIL") || allStatuses.includes("ERROR")
      ? "failed"
      : coreStatuses.length > 0 && coreStatuses.every((s) => s === "PASS" || s === "OK")
        ? "verified"
        : claims.length > 0
          ? "partial"
          : "none"
    : summaryVerdict;
  const VERDICT: Record<string, { cls: string; label: string }> = {
    verified: { cls: "border-emerald-500/30 bg-emerald-500/10 text-emerald-600", label: "Verified" },
    failed: { cls: "border-rose-500/30 bg-rose-500/10 text-rose-600", label: "Failed" },
    partial: { cls: "border-amber-500/30 bg-amber-500/10 text-amber-600", label: "Partial" },
    none: { cls: "border-border bg-surface-muted text-foreground-muted", label: "No claims" },
  };
  const verdict = VERDICT[effectiveVerdict] ?? VERDICT.none;

  return (
    <li className="border-b border-border last:border-0">
      <button
        type="button"
        onClick={toggle}
        className="flex w-full items-center gap-3 px-4 py-3 text-left hover:bg-surface-muted/40"
      >
        <span className="w-12 shrink-0 font-mono text-xs tabular-nums text-foreground-muted">{summarySeq ?? "—"}</span>
        <span className="min-w-0 flex-1 truncate font-mono text-sm font-medium">{summaryTask}</span>
        {summaryCreated && (
          <span className="hidden shrink-0 text-xs text-foreground-muted sm:inline" title={summaryCreated}>
            {new Date(summaryCreated).toLocaleDateString(undefined, { month: "short", day: "numeric" })}
          </span>
        )}
        <span className={`shrink-0 rounded-full border px-2.5 py-0.5 text-xs font-medium ${verdict.cls}`}>{verdict.label}</span>
        <span className="shrink-0 text-foreground-muted transition" style={{ transform: open ? "rotate(180deg)" : "none" }} aria-hidden>⌄</span>
      </button>

      {open && (
        <div className="space-y-4 border-t border-border bg-surface-muted/20 px-4 py-4">
          {loading ? (
            <p className="text-xs text-foreground-muted">Loading the full attestation…</p>
          ) : loadError ? (
            <p className="text-xs text-foreground-muted">{loadError}</p>
          ) : !receipt ? (
            <p className="text-xs text-foreground-muted">This receipt&rsquo;s full attestation couldn&rsquo;t be loaded.</p>
          ) : (
            <>
              {/* What it attests — the claim classes in plain language. */}
              <div>
                <p className="mb-2 text-xs font-semibold uppercase tracking-wide text-foreground-muted">What this receipt attests</p>
                <ul className="space-y-2">
                  {claims.map((c) => {
                    const t = claimTone(c.status);
                    return (
                      <li key={c.class} className="rounded-lg border border-border bg-surface p-3">
                        <div className="flex items-center justify-between gap-3">
                          <span className="text-xs font-medium">{CLAIM_LABEL[c.class] ?? c.class}</span>
                          <span className={`shrink-0 rounded border px-1.5 py-0.5 font-mono text-[11px] font-medium ${t.cls}`}>{t.label}</span>
                        </div>
                        <p className="mt-1 text-xs leading-relaxed text-foreground-muted">{c.detail}</p>
                      </li>
                    );
                  })}
                </ul>
              </div>

              {/* Provenance facts. */}
              <dl className="grid gap-x-6 gap-y-2 text-xs sm:grid-cols-2">
                <div>
                  <dt className="text-foreground-muted">Signing scheme</dt>
                  <dd className="font-mono">{receipt.scheme}</dd>
                </div>
                <div>
                  <dt className="text-foreground-muted">Inclusion-log position</dt>
                  <dd className="font-mono">#{receipt.inclusion_seq ?? "—"}</dd>
                </div>
                <div className="sm:col-span-2">
                  <dt className="text-foreground-muted">Signed by (receipt-signing key)</dt>
                  <dd className="font-mono break-all">{receipt.key_id}</dd>
                </div>
              </dl>

              {/* Independent verification — performed live in the browser via
                  the backend, against the published key anchor. No CLI needed. */}
              <div>
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div>
                    <p className="text-xs font-semibold">Verify this receipt</p>
                    <p className="text-[11px] text-foreground-muted">
                      Re-checks the signature, the trust-envelope binding, and the signing key against the cluster&rsquo;s published anchor — independently of this screen.
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={runVerify}
                    disabled={verifying}
                    className="shrink-0 rounded-lg bg-signal px-3.5 py-2 text-xs font-semibold text-signal-fg hover:opacity-90 disabled:opacity-50"
                  >
                    {verifying ? "Verifying…" : result ? "Re-verify" : "Verify now"}
                  </button>
                </div>

                {verifyError && <p className="mt-2 text-xs text-danger">{verifyError}</p>}

                {result && (
                  <div className="mt-3 space-y-3 rounded-lg border border-border bg-surface p-3">
                    <div className="flex items-center gap-2">
                      <span
                        className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-semibold ${
                          result.verified
                            ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
                            : "border-rose-500/30 bg-rose-500/10 text-rose-600"
                        }`}
                      >
                        {result.verified ? "✓ Verified" : "✗ Not verified"}
                      </span>
                      <span className="text-[11px] text-foreground-muted">
                        {result.verified ? "authentic, unaltered, chained, and checkpoint-pinned" : "one or more checks failed"}
                      </span>
                    </div>

                    {/* Each check shows the recorded value next to the value the
                        backend independently recomputed — the auditor SEES them
                        match, not just a green tick. */}
                    <ul className="space-y-2">
                      {result.checks.map((c) => (
                        <li key={c.name} className="text-xs">
                          <div className="flex gap-2">
                            <span aria-hidden className={c.advisory ? "text-foreground-muted" : c.passed ? "text-emerald-600" : "text-rose-600"}>
                              {c.advisory ? "ℹ" : c.passed ? "✓" : "✗"}
                            </span>
                            <span className="min-w-0">
                              <span className="font-medium">{c.name}</span>
                              <span className="text-foreground-muted"> — {c.detail}</span>
                              {c.advisory && (
                                <span className="ml-1 rounded bg-surface-muted px-1 py-0.5 text-[10px] font-medium text-foreground-muted">shown, not verified</span>
                              )}
                            </span>
                          </div>
                          {(c.expected || c.computed) && (
                            <dl className="ml-5 mt-1 space-y-0.5 font-mono text-[10px] text-foreground-muted">
                              {c.expected && (
                                <div className="flex gap-1.5">
                                  <dt className="shrink-0 w-20 not-italic">recorded</dt>
                                  <dd className="truncate" title={c.expected}>{c.expected}</dd>
                                </div>
                              )}
                              {c.computed && (
                                <div className="flex gap-1.5">
                                  <dt className="shrink-0 w-20">recomputed</dt>
                                  <dd className={`truncate ${c.passed && c.expected === c.computed ? "text-emerald-600" : ""}`} title={c.computed}>{c.computed}</dd>
                                </div>
                              )}
                            </dl>
                          )}
                        </li>
                      ))}
                    </ul>

                    <EvidencePanel result={result} task={receipt.task} />
                  </div>
                )}
                {compliance && <CompliancePackView pack={compliance} />}
              </div>
            </>
          )}
        </div>
      )}
    </li>
  );
}

/** The raw artifacts behind the verdict — the signed statement an auditor can
 *  read & download, the inclusion-proof entry, and the signed checkpoint. */
function EvidencePanel({ result, task }: { result: VerifyResult; task: string }) {
  const ev = result.evidence;
  if (!ev) return null;
  const statementStr = ev.signed_statement ? JSON.stringify(ev.signed_statement, null, 2) : null;

  function download() {
    if (!statementStr) return;
    const blob = new Blob([statementStr], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${task}.receipt-statement.json`;
    a.click();
    URL.revokeObjectURL(url);
  }

  return (
    <details className="group rounded-lg border border-border bg-surface-muted/30">
      <summary className="flex cursor-pointer items-center justify-between px-3 py-2 text-xs font-semibold">
        Evidence — the artifacts this verdict was computed over
        <span className="text-foreground-muted transition group-open:rotate-180" aria-hidden>⌄</span>
      </summary>
      <div className="space-y-3 border-t border-border px-3 py-3">
        {/* The signed payload — the actual evidence for integrity & the claims. */}
        {statementStr && (
          <div>
            <div className="mb-1 flex items-center justify-between">
              <p className="text-[11px] font-semibold">Signed statement (the exact bytes the signature covers)</p>
              <button type="button" onClick={download} className="rounded border border-border px-2 py-0.5 text-[10px] font-medium hover:bg-surface">
                Download JSON
              </button>
            </div>
            <pre className="max-h-56 overflow-auto rounded border border-border bg-surface px-2.5 py-2 font-mono text-[10px] leading-relaxed">{statementStr}</pre>
          </div>
        )}

        {/* Signature material. */}
        <dl className="grid gap-1.5 text-[10px] sm:grid-cols-2">
          <EvFact label="Scheme" value={ev.scheme ?? undefined} />
          <EvFact label="Anchor key (trusted)" value={ev.anchor_key_id ?? undefined} mono />
          <EvFact label="Anchor public key (b64)" value={ev.anchor_public_key_b64 ?? undefined} mono wide />
          <EvFact label="Signature (b64)" value={ev.signature_b64 ?? undefined} mono wide />
        </dl>

        {/* Inclusion proof. */}
        {ev.inclusion && (
          <div className="rounded border border-border bg-surface p-2.5">
            <p className="mb-1 text-[11px] font-semibold">Inclusion proof — position #{ev.inclusion.seq} of {ev.inclusion.tree_size}</p>
            <dl className="grid gap-1 font-mono text-[10px]">
              <EvFact label="payload sha256" value={ev.inclusion.payload_sha256} mono wide />
              <EvFact label="prev hash" value={ev.inclusion.prev_hash} mono wide />
              <EvFact label="entry hash" value={ev.inclusion.entry_hash} mono wide />
              <EvFact label="recomputed" value={ev.inclusion.recomputed_entry_hash} mono wide match={ev.inclusion.entry_hash === ev.inclusion.recomputed_entry_hash} />
              <EvFact label="chain head" value={ev.inclusion.chain_head} mono wide />
            </dl>
            <p className="mt-1 text-[10px] text-foreground-muted">
              {ev.inclusion.chain_consistent ? "✓ All entries recompute genesis → head." : "✗ Chain inconsistent."}
            </p>
          </div>
        )}

        {/* Signed checkpoint + witness. */}
        {ev.checkpoint && (
          <div className="rounded border border-border bg-surface p-2.5">
            <p className="mb-1 text-[11px] font-semibold">
              Signed checkpoint {ev.checkpoint.signature_valid ? <span className="text-emerald-600">✓ signature valid</span> : <span className="text-rose-600">✗ invalid</span>}
            </p>
            <dl className="grid gap-1 font-mono text-[10px]">
              <EvFact label="tree size" value={String(ev.checkpoint.tree_size)} mono />
              <EvFact label="root hash" value={ev.checkpoint.root_hash} mono wide />
              <EvFact label="signed note" value={ev.checkpoint.signed_note.replace(/\n/g, "\\n")} mono wide />
              <EvFact label="signature (b64)" value={ev.checkpoint.signature_b64} mono wide />
              {ev.checkpoint.witness_key_id && <EvFact label="witness key (independent)" value={ev.checkpoint.witness_key_id} mono wide />}
            </dl>
          </div>
        )}
      </div>
    </details>
  );
}

function EvFact({ label, value, mono, wide, match }: { label: string; value?: string; mono?: boolean; wide?: boolean; match?: boolean }) {
  if (!value) return null;
  return (
    <div className={`flex gap-1.5 ${wide ? "sm:col-span-2" : ""}`}>
      <dt className="shrink-0 text-foreground-muted">{label}</dt>
      <dd className={`min-w-0 truncate ${mono ? "font-mono" : ""} ${match ? "text-emerald-600" : ""}`} title={value}>{value}</dd>
    </div>
  );
}
