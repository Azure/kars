// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge — the shared Auditor view. Rendered both inside the Operator
// Console (/console/audit) and on the dedicated read-only Auditor surface
// (/audit), so the two never drift. It is entirely read-only: a chain-integrity
// verdict, the four claim classes, and every Governance Receipt as an
// inspectable, independently-verifiable unit.

import { Section, Stat, Badge } from "@/components/ui";
import { BffError } from "@/lib/bff";
import { Icon, type IconName } from "@/components/icon";
import { AuditSearch } from "@/app/console/audit/audit-search";
import type { Audit } from "@/lib/types";

const CLAIMS: { icon: IconName; title: string; q: string; d: string }[] = [
  { icon: "seal", title: "Integrity", q: "Authentic & unaltered?", d: "Verify the receipt's signature, payload binding, and signing key; recorded claims alone are not verification." },
  { icon: "target", title: "Conformance", q: "Stayed within authority?", d: "Inspect the run's claims and supporting evidence for tier, tool policy, budget, and egress enforcement." },
  { icon: "check-cycle", title: "Completeness", q: "All controls enforced & recorded?", d: "Check for every applicable control's evidence. Missing or omitted required claims cannot count as passing." },
  { icon: "scale", title: "Regulatory", q: "Anchored & witnessed?", d: "Inspect the checkpoint's trust anchor and witness metadata. Witness presence alone is not independent verification." },
];

export function AuditView({ audit, error }: { audit: Audit | null; error: unknown }) {
  const status = error instanceof BffError ? error.status : null;
  const failureTitle = status === 401 ? "Sign in to view audit evidence"
    : status === 403 ? "Audit access denied"
    : "Audit evidence unavailable";
  const failureDetail = status === 401
    ? "Your session is missing or expired. Sign in again to view audit evidence."
    : status === 403
      ? "Your current account does not have audit access. Use an account with the auditor or admin role."
      : "The audit records could not be loaded. This does not mean the log is empty or its integrity has failed. Refresh to retry.";
  const missingSeq = audit ? audit.receipts.filter((r) => r.inclusion_seq == null).length : 0;
  // Whole-log verification does not bind the listed receipts to its entries.
  const integrity = audit?.integrity;
  const chainOk =
    !!integrity &&
    integrity.chain_consistent &&
    integrity.checkpoint_verified &&
    integrity.tree_size > 0;
  const witnessPresent = !!integrity?.witness_present;
  const anchorPinned = !!integrity?.anchor_pinned;

  return (
    <div className="space-y-6">
      {/* What an auditor can verify here — the four claim classes, up front. */}
      <section aria-label="What you can verify" className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
        {CLAIMS.map((c) => (
          <div key={c.title} className="rounded-xl border border-border bg-surface p-4">
            <p className="flex items-center gap-1.5 text-sm font-semibold"><Icon name={c.icon} className="text-signal" /> {c.title}</p>
            <p className="mt-0.5 text-[11px] font-medium text-foreground-muted">{c.q}</p>
            <p className="mt-1.5 text-xs text-foreground-muted">{c.d}</p>
          </div>
        ))}
      </section>

      {/* Chain-integrity verdict — the auditor's first question, answered. */}
      {audit && !error ? (
        <section
          className={`kb-rise flex flex-wrap items-start justify-between gap-4 rounded-xl border p-5 ${
            chainOk ? "border-emerald-500/30 bg-emerald-500/[0.05]" : "border-amber-500/30 bg-amber-500/[0.05]"
          }`}
        >
          <div className="flex items-start gap-3">
            <span aria-hidden className={`grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-surface ${chainOk ? "text-ok" : "text-warning"}`}>
              <Icon name={chainOk ? "shield" : "target"} size={20} />
            </span>
            <div>
              <p className="text-base font-semibold">
                {chainOk
                  ? witnessPresent
                    ? "Log verified — chain recomputed, checkpoint signature valid, witness co-signature present"
                    : "Log verified — chain recomputed and checkpoint signature valid"
                  : "Log not fully verified — see details"}
              </p>
              <p className="mt-0.5 max-w-2xl text-sm text-foreground-muted">
                {chainOk
                  ? `The inclusion log's ${integrity.tree_size} entries recompute genesis → head, and the checkpoint's Ed25519 signature verifies${anchorPinned ? " against an out-of-band-pinned anchor. This verifies the supplied snapshot, not its freshness or protection against replay of an older signed log." : " against the cluster's published key, in the same trust domain as the log. This is not an independent trust anchor."}${witnessPresent ? " Reported witness metadata is present; its identity and co-signature are not verified here." : ""}`
                  : integrity && !integrity.chain_consistent && integrity.tree_size > 0
                    ? "The inclusion log did NOT recompute cleanly — an entry hash, sequence, or prev-hash link does not check out. The history is not tamper-evident; investigate before trusting these receipts."
                    : integrity && integrity.chain_consistent && !integrity.checkpoint_verified
                      ? "The chain recomputes, but the signed checkpoint's signature did not verify against the published anchor (or no checkpoint is published). Until the log head is witnessed by a valid signed checkpoint, the history is not fully tamper-evident."
                      : "No nonempty, cryptographically verified log snapshot is available."}
              </p>
              <p className="mt-2 max-w-2xl text-xs text-foreground-muted">
                This log check does not verify each listed receipt&apos;s signature, payload binding, inclusion, or control enforcement. Expand a receipt and use Verify now for its cryptographic checks.
                {missingSeq > 0 && ` ${missingSeq} receipt(s) lack a recorded inclusion-log position.`}
              </p>
            </div>
          </div>
          <Badge tone={chainOk ? "ok" : "warn"} dot>
            {chainOk ? (anchorPinned ? "Pinned signature verified" : "Consistent + signed") : "Not verified"}
          </Badge>
        </section>
      ) : null}

      {error || !audit ? (
        <div role="alert" className="rounded-xl border border-danger/40 bg-danger/10 p-5 text-sm">
          <p className="font-semibold">{failureTitle}</p>
          <p className="mt-2 text-foreground-muted">{failureDetail}</p>
          <a href="/audit" className="mt-3 inline-block text-signal underline">Reopen audit view</a>
        </div>
      ) : (
        <>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
            <Stat label="Receipts issued" value={audit.receipts.length} />
            <Stat label="Inclusion-log entries" value={audit.inclusion_log_size} />
            <div className="rounded-xl border border-signal/30 bg-signal/[0.05] p-4">
              {audit.checkpoint ? (
                <>
                  <p className="inline-flex items-center gap-1.5 text-sm font-semibold text-signal">
                    <Icon name="shield" /> Recorded checkpoint
                  </p>
                  <p className="mt-1 font-mono text-xs text-foreground-muted">
                    tree size {audit.checkpoint.tree_size} · root {audit.checkpoint.root_hash.slice(0, 16)}…
                  </p>
                </>
              ) : (
                <p className="text-sm text-foreground-muted">No checkpoint yet</p>
              )}
            </div>
          </div>

          <Section
            title="Governance receipts"
            subtitle="Search recorded receipts or filter their claims. Claim statuses and log positions are recorded metadata, not per-receipt cryptographic verdicts. Expand a row to verify its evidence."
            action={<Badge tone="muted">{audit.receipts.length}</Badge>}
          >
            <AuditSearch receipts={audit.receipts} />
          </Section>

          {/* How to read a receipt — the auditor's primer, once. */}
          <Section title="How to read a receipt">
            <ul className="space-y-2 text-sm text-foreground-muted">
              <li className="flex gap-2">
                <span aria-hidden>①</span>
                <span><strong className="text-foreground">Recorded claims</strong> — &ldquo;Claims pass&rdquo; requires passing <em>integrity</em>, <em>conformance</em>, and <em>completeness</em> claims. It does not mean their assertions have been independently verified. <em>Regulatory</em> maturity is advisory and shown separately.</span>
              </li>
              <li className="flex gap-2">
                <span aria-hidden>②</span>
                <span><strong className="text-foreground">Inclusion log</strong> — a position number is a recorded pointer, not proof of membership. Per-receipt verification must bind the signed payload to the corresponding log entry and checkpoint.</span>
              </li>
              <li className="flex gap-2">
                <span aria-hidden>③</span>
                <span><strong className="text-foreground">Verify yourself</strong> — hit <em>Verify now</em> on any row. The backend re-checks the Ed25519 signature, the trust-envelope binding, and the signing key against the cluster&apos;s published anchor, live — no tooling to install, and no trust in this screen required.</span>
              </li>
              <li className="flex gap-2">
                <span aria-hidden>④</span>
                <span><strong className="text-foreground">Why claims are partial</strong> — a required claim is missing, omitted, unknown, or reports incomplete evidence. Inspect the claim details and retained evidence; do not infer enforcement from a passing signature check. Regulatory maturity alone does not prevent &ldquo;Claims pass&rdquo;.</span>
              </li>
            </ul>
          </Section>
        </>
      )}
    </div>
  );
}
