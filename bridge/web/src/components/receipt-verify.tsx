"use client";

// kars Bridge — in-browser receipt verification for the mission surface. The
// receipt panel shows the `kars receipt verify` CLI as the expert option; this
// lets an ordinary user verify independently right here (audit f40): it calls
// the same backend verify endpoint (which re-checks the Ed25519 signature, the
// trust-envelope binding, the signing key against the cluster's published
// anchor, and the inclusion-log entry) and renders every recomputed check — so
// the user SEES the proof, not just a green tick.

import { useState } from "react";
import type { VerifyResult } from "@/lib/types";

export function ReceiptVerifyButton({ ns, task }: { ns: string; task: string }) {
  const [verifying, setVerifying] = useState(false);
  const [result, setResult] = useState<VerifyResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function run() {
    setVerifying(true);
    setError(null);
    setResult(null);
    try {
      const res = await fetch(
        `/api/namespaces/${encodeURIComponent(ns)}/tasks/${encodeURIComponent(task)}/receipt/verify`,
        { method: "POST", headers: { accept: "application/json" } },
      );
      if (!res.ok) throw new Error(`verify failed: ${res.status}`);
      setResult((await res.json()) as VerifyResult);
    } catch {
      setError("Verification couldn't be completed — the audit backend may be unreachable.");
    } finally {
      setVerifying(false);
    }
  }

  return (
    <section className="rounded-xl border border-border bg-surface p-5">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h3 className="text-sm font-semibold">Verify this receipt</h3>
          <p className="mt-0.5 text-[11px] text-foreground-muted">
            Re-checks the signature, the trust-envelope binding, and the signing key against the
            cluster&rsquo;s published anchor — live, in your browser. No tooling to install.
          </p>
        </div>
        <button
          type="button"
          onClick={run}
          disabled={verifying}
          className="shrink-0 rounded-lg bg-signal px-3.5 py-2 text-xs font-semibold text-signal-fg hover:opacity-90 disabled:opacity-50"
        >
          {verifying ? "Verifying…" : result ? "Re-verify" : "Verify now"}
        </button>
      </div>

      {error && <p className="mt-2 text-xs text-danger">{error}</p>}

      {result && (
        <div className="mt-3 space-y-3 rounded-lg border border-border bg-surface-muted/30 p-3">
          <span
            className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-semibold ${
              result.verified
                ? "border-ok/30 bg-ok/10 text-ok"
                : "border-danger/30 bg-danger/10 text-danger"
            }`}
          >
            {result.verified ? "✓ Verified" : "✗ Not verified"}
          </span>
          <ul className="space-y-2">
            {result.checks.map((c) => (
              <li key={c.name} className="text-xs">
                <div className="flex gap-2">
                  <span
                    aria-hidden
                    className={c.advisory ? "text-foreground-muted" : c.passed ? "text-ok" : "text-danger"}
                  >
                    {c.advisory ? "ℹ" : c.passed ? "✓" : "✗"}
                  </span>
                  <span className="min-w-0">
                    <span className="font-medium">{c.name}</span>
                    {c.advisory && (
                      <span className="ml-1 rounded bg-surface-muted px-1 py-0.5 text-[10px] font-medium text-foreground-muted">
                        shown, not verified
                      </span>
                    )}
                    <span className="text-foreground-muted"> — {c.detail}</span>
                  </span>
                </div>
                {(c.expected || c.computed) && (
                  <dl className="ml-5 mt-1 space-y-0.5 font-mono text-[10px] text-foreground-muted">
                    {c.expected && (
                      <div className="flex gap-1.5">
                        <dt className="w-20 shrink-0">{c.advisory ? "recorded key" : "recorded"}</dt>
                        <dd className="truncate" title={c.expected}>{c.expected}</dd>
                      </div>
                    )}
                    {c.computed && (
                      <div className="flex gap-1.5">
                        <dt className="w-20 shrink-0">recomputed</dt>
                        <dd className={`truncate ${c.passed && c.expected === c.computed ? "text-ok" : ""}`} title={c.computed}>{c.computed}</dd>
                      </div>
                    )}
                  </dl>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
