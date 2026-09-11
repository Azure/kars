"use client";

// kars Bridge — Governance Receipt evidence panel (the auditor's moment).
//
// This renders the signed receipt the controller emitted: the claim matrix,
// the signing identity, and the signed in-toto predicate. It is deliberately
// HONEST about its own role — it renders evidence, it does not assert
// cryptographic validity. The "Verify" affordance shows the exact
// `kars receipt verify` command, because independent verification (against the
// controller's out-of-band public-key anchor) is a different trust domain than
// this UI. Showing a self-asserted green "verified" checkmark here would be the
// very dashboard-trust the receipt exists to replace.

import { useState } from "react";
import type { Receipt, ReceiptClaim } from "@/lib/types";

function ClaimBadge({ status }: { status: string }) {
  const map: Record<string, { cls: string; label: string }> = {
    PASS: { cls: "border-ok/40 bg-ok/10 text-ok", label: "PASS" },
    PARTIAL: { cls: "border-warning/40 bg-warning/10 text-warning", label: "PARTIAL" },
    OMITTED: {
      cls: "border-border bg-surface-muted text-foreground-muted",
      label: "OMITTED",
    },
    FAIL: { cls: "border-danger/40 bg-danger/10 text-danger", label: "FAIL" },
  };
  const s = map[status] ?? {
    cls: "border-border bg-surface-muted text-foreground-muted",
    label: status,
  };
  return (
    <span
      className={`inline-flex min-w-[4.5rem] justify-center rounded border px-2 py-0.5 font-mono text-[11px] font-medium tracking-wide ${s.cls}`}
    >
      {s.label}
    </span>
  );
}

function CopyButton({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(value);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        } catch {
          // insecure context — value remains selectable
        }
      }}
      aria-label={label}
      className="shrink-0 border-l border-border px-3 text-xs font-medium text-foreground-muted hover:bg-surface hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
    >
      {copied ? "Copied" : "Copy"}
    </button>
  );
}

function claimTitle(claim: ReceiptClaim): string {
  const t: Record<string, string> = {
    integrity: "Integrity",
    conformance: "Conformance",
    completeness: "Completeness",
    regulatory: "Regulatory",
  };
  return t[claim.class] ?? claim.class;
}

export function ReceiptPanel({ receipt }: { receipt: Receipt }) {
  const [showPayload, setShowPayload] = useState(false);
  const sig = receipt.signatures[0];

  // The validated launch package recorded at the head of the predicate (§20).
  const lp = (() => {
    const s = receipt.statement as { predicate?: { launchPackage?: Record<string, unknown> } } | null;
    return s?.predicate?.launchPackage ?? null;
  })();

  return (
    <section
      aria-labelledby="receipt-heading"
      className="rounded-xl border border-border bg-surface p-6"
    >
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2 id="receipt-heading" className="text-sm font-semibold">
            Governance Receipt
          </h2>
          <p className="mt-0.5 text-xs text-foreground-muted">
            A signed, independently verifiable record that this task was
            governed under its trust envelope.
          </p>
        </div>
        <span className="inline-flex items-center gap-1.5 rounded-full border border-signal/40 bg-signal/10 px-2.5 py-1 text-xs font-medium text-signal">
          <svg aria-hidden viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="currentColor">
            <path d="M8 1 2.5 3.2v3.5c0 3.2 2.3 6.2 5.5 7.3 3.2-1.1 5.5-4.1 5.5-7.3V3.2L8 1Zm-.9 9.6L4.6 8.1l1-1 1.5 1.5 3-3 1 1-4 4Z" />
          </svg>
          Signed · {receipt.scheme}
        </span>
      </div>

      {/* Launch package — what was reviewed + approved, at the head (§20). */}
      {lp && (
        <div className="mt-5 rounded-lg border border-border bg-surface-muted/40 p-4">
          <p className="text-xs font-medium uppercase tracking-wide text-foreground-muted">
            Validated launch package
          </p>
          <dl className="mt-2 flex flex-wrap gap-x-6 gap-y-1 text-xs">
            {typeof lp.model === "string" && (
              <div className="flex gap-1.5">
                <dt className="text-foreground-muted">Model</dt>
                <dd className="font-medium">{lp.model}</dd>
              </div>
            )}
            {typeof lp.runtime === "string" && (
              <div className="flex gap-1.5">
                <dt className="text-foreground-muted">Harness</dt>
                <dd className="font-medium">{lp.runtime}</dd>
              </div>
            )}
            {typeof lp.toolPolicy === "string" && (
              <div className="flex gap-1.5">
                <dt className="text-foreground-muted">Tool policy</dt>
                <dd className="font-medium">{lp.toolPolicy}</dd>
              </div>
            )}
            {typeof lp.isolation === "string" && (
              <div className="flex gap-1.5">
                <dt className="text-foreground-muted">Isolation</dt>
                <dd className="font-medium">{lp.isolation}</dd>
              </div>
            )}
          </dl>
          {typeof lp.digest === "string" && (
            <p className="mt-2 font-mono text-[10px] text-foreground-muted">{lp.digest}</p>
          )}
        </div>
      )}


      {/* Claim matrix — the honest §24b posture, surfaced verbatim. */}
      <div className="mt-5">
        <p className="text-xs font-medium uppercase tracking-wide text-foreground-muted">
          Claim matrix
        </p>
        <ul className="mt-2 divide-y divide-border border-t border-border">
          {receipt.claims.map((c) => (
            <li key={c.class} className="flex items-start gap-3 py-3">
              <ClaimBadge status={c.status} />
              <div className="min-w-0">
                <p className="text-sm font-medium">{claimTitle(c)}</p>
                <p className="mt-0.5 text-xs text-foreground-muted">{c.detail}</p>
              </div>
            </li>
          ))}
        </ul>
      </div>

      {/* Signing identity. */}
      <dl className="mt-5 space-y-3">
        <div>
          <dt className="text-xs text-foreground-muted">Signed by (key id)</dt>
          <dd className="mt-1 flex items-stretch overflow-hidden rounded-lg border border-border bg-surface-muted">
            <code className="min-w-0 flex-1 break-all px-3 py-2 font-mono text-xs leading-relaxed">
              {receipt.key_id}
            </code>
            <CopyButton value={receipt.key_id} label="Copy key id" />
          </dd>
        </div>
        {sig && (
          <div>
            <dt className="text-xs text-foreground-muted">
              Signature ({receipt.payload_type})
            </dt>
            <dd className="mt-1 flex items-stretch overflow-hidden rounded-lg border border-border bg-surface-muted">
              <code className="min-w-0 flex-1 break-all px-3 py-2 font-mono text-xs leading-relaxed">
                {sig.sig}
              </code>
              <CopyButton value={sig.sig} label="Copy signature" />
            </dd>
          </div>
        )}
        {receipt.inclusion_state === "Failed" && (
          <div className="rounded-lg border border-danger/40 bg-danger/5 p-3">
            <dt className="text-xs font-medium text-danger">
              Transparency inclusion failed
            </dt>
            <dd className="mt-1 text-xs text-foreground-muted">
              This receipt is signed, but it is not currently checkpointed and
              independently witnessed.{" "}
              {receipt.inclusion_error ?? "The controller reported an inclusion failure."}
            </dd>
          </div>
        )}
        {receipt.inclusion_state !== "Failed" && receipt.inclusion_seq != null && (
          <div>
            <dt className="text-xs text-foreground-muted">
              Inclusion log (cross-receipt tamper-evidence)
            </dt>
            <dd className="mt-1 flex items-center gap-2 text-sm">
              <span className="inline-flex items-center rounded-full border border-border bg-surface-muted px-2 py-0.5 font-mono text-xs">
                seq {receipt.inclusion_seq}
              </span>
              {receipt.log_segment && (
                <span className="font-mono text-xs text-foreground-muted">
                  {receipt.log_segment}
                </span>
              )}
              {receipt.inclusion_entry_hash && (
                <code className="truncate font-mono text-xs text-foreground-muted">
                  {receipt.inclusion_entry_hash.slice(0, 24)}…
                </code>
              )}
            </dd>
            <p className="mt-1 text-xs text-foreground-muted">
              Recorded in the segmented hash-chained receipt log.
              {receipt.checkpoint
                ? " A signed checkpoint is available for independent verification."
                : ""}
              {receipt.witnessed
                ? " A witness co-signature is recorded; it is shown, not independently verified here."
                : ""}
            </p>
            {receipt.checkpoint && (
              <p className="mt-1.5 text-xs text-foreground-muted">
                <span className="font-medium text-foreground">Signed checkpoint</span>{" "}
                over {receipt.checkpoint.tree_size} entries · root{" "}
                <code className="font-mono">
                  {receipt.checkpoint.root_hash.slice(0, 16)}…
                </code>{" "}
                — pin this to detect a later history rewrite (
                <code className="font-mono">kars receipt checkpoint</code>).
              </p>
            )}
          </div>
        )}
      </dl>

      {/* Independent verification — the whole point. */}
      <div className="mt-5 rounded-lg border border-signal/30 bg-signal/5 p-4">
        <p className="text-sm font-medium">Verify independently</p>
        <p className="mt-1 text-xs text-foreground-muted">
          Don&apos;t trust this screen. Verify the signature against the
          controller&apos;s published public key — on a plain kars cluster, no
          Bridge required:
        </p>
        <div className="mt-2 flex items-stretch overflow-hidden rounded-lg border border-border bg-surface">
          <code className="min-w-0 flex-1 break-all px-3 py-2 font-mono text-xs leading-relaxed">
            {receipt.verify_command}
          </code>
          <CopyButton value={receipt.verify_command} label="Copy verify command" />
        </div>
      </div>

      {/* The signed payload — the exact bytes the signature covers. */}
      <div className="mt-4">
        <button
          type="button"
          onClick={() => setShowPayload((v) => !v)}
          aria-expanded={showPayload}
          className="rounded text-xs font-medium text-signal hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-signal"
        >
          {showPayload ? "Hide" : "Show"} signed in-toto predicate
        </button>
        {showPayload && (
          <pre className="mt-2 max-h-80 overflow-auto rounded-lg border border-border bg-surface-muted p-3 font-mono text-[11px] leading-relaxed">
            {JSON.stringify(receipt.statement, null, 2)}
          </pre>
        )}
      </div>
    </section>
  );
}
