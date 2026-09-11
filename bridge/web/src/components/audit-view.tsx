// kars Bridge — the shared Auditor view. Rendered both inside the Operator
// Console (/console/audit) and on the dedicated read-only Auditor surface
// (/audit), so the two never drift. It is entirely read-only: a chain-integrity
// verdict, the four claim classes, and every Governance Receipt as an
// inspectable, independently-verifiable unit.

import { Section, Stat, Badge } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { Icon, type IconName } from "@/components/icon";
import { AuditSearch } from "@/app/console/audit/audit-search";
import type { Audit } from "@/lib/types";

const CLAIMS: { icon: IconName; title: string; q: string; d: string }[] = [
  { icon: "seal", title: "Integrity", q: "Authentic & unaltered?", d: "Each receipt is Ed25519-signed and its signing key checks against the cluster's published anchor." },
  { icon: "target", title: "Conformance", q: "Stayed within authority?", d: "The run never exceeded its envelope — tier, tool policy, budget, and egress were enforced." },
  { icon: "check-cycle", title: "Completeness", q: "All controls enforced & recorded?", d: "Every governance control that should have run did, and left a durable, re-derivable record." },
  { icon: "scale", title: "Regulatory", q: "Anchored & witnessed?", d: "The receipt sits in a hash-chained inclusion log an independent checkpoint co-signs." },
];

export function AuditView({ audit, error }: { audit: Audit | null; error: boolean }) {
  const haveCheckpoint = !!audit?.checkpoint;
  const missingSeq = audit ? audit.receipts.filter((r) => r.inclusion_seq == null).length : 0;
  const allIncluded = !!audit && missingSeq === 0;
  // The REAL verdict comes from server-side cryptographic verification: the whole
  // hash chain recomputed + the signed checkpoint verified against the published
  // anchor. Field presence (inclusion_seq set, checkpoint CM exists) is NOT
  // verification — a green banner must mean the crypto actually checked out.
  const integrity = audit?.integrity;
  const chainOk =
    !!integrity &&
    integrity.chain_consistent &&
    integrity.checkpoint_verified &&
    allIncluded &&
    (audit?.receipts.length ?? 0) > 0;
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
      {audit && !error && (
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
                  ? `All ${audit.receipts.length} receipts sit in a hash-chained inclusion log of ${audit.inclusion_log_size} entries that recomputes genesis → head, and the signed checkpoint's Ed25519 signature verifies${anchorPinned ? " against an out-of-band-pinned anchor — so no entry can be altered, removed, or forked without detection." : " against the cluster's published anchor key. Because that anchor lives in the same trust domain as the log, this proves consistency and authenticity for anyone outside that domain; for independent tamper-evidence, verify against an out-of-band-pinned key (kars receipt verify)."}${witnessPresent ? " An independent transparency-witness co-signature is present (shown, not re-verified in V0)." : ""}`
                  : integrity && !integrity.chain_consistent && integrity.tree_size > 0
                    ? "The inclusion log did NOT recompute cleanly — an entry hash, sequence, or prev-hash link does not check out. The history is not tamper-evident; investigate before trusting these receipts."
                    : integrity && integrity.chain_consistent && !integrity.checkpoint_verified
                      ? "The chain recomputes, but the signed checkpoint's signature did not verify against the published anchor (or no checkpoint is published). Until the log head is witnessed by a valid signed checkpoint, the history is not fully tamper-evident."
                      : missingSeq > 0
                        ? `${missingSeq} of ${audit.receipts.length} receipt(s) have no inclusion-log position yet${haveCheckpoint ? "" : " and no signed checkpoint is published"}. Until every receipt is chained${haveCheckpoint ? "" : " and a checkpoint is published"}, the history is not fully tamper-evident.`
                        : "No signed checkpoint is published yet. Until the log head is witnessed by a signed checkpoint, the history is not fully tamper-evident."}
              </p>
            </div>
          </div>
          <Badge tone={chainOk ? "ok" : "warn"} dot>
            {chainOk ? (anchorPinned ? "Tamper-evident" : "Consistent + signed") : "Not yet sealed"}
          </Badge>
        </section>
      )}

      {error || !audit ? (
        <HonestState variant="not_wired" title="Audit substrate unreachable" detail="The Bridge backend can't reach the audit log right now." />
      ) : (
        <>
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
            <Stat label="Receipts issued" value={audit.receipts.length} />
            <Stat label="Inclusion-log entries" value={audit.inclusion_log_size} />
            <div className="rounded-xl border border-signal/30 bg-signal/[0.05] p-4">
              {audit.checkpoint ? (
                <>
                  <p className="inline-flex items-center gap-1.5 text-sm font-semibold text-signal">
                    <Icon name="shield" /> Signed checkpoint
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
            subtitle="Search a run or agent to pull its receipts and chain of custody, or filter by verdict. Expand any row to see exactly what was checked and how to verify it yourself."
            action={<Badge tone="muted">{audit.receipts.length}</Badge>}
          >
            <AuditSearch receipts={audit.receipts} />
          </Section>

          {/* How to read a receipt — the auditor's primer, once. */}
          <Section title="How to read a receipt">
            <ul className="space-y-2 text-sm text-foreground-muted">
              <li className="flex gap-2">
                <span aria-hidden>①</span>
                <span><strong className="text-foreground">Verdict</strong> — the required verification classes are <em>integrity</em> (authentic &amp; unaltered), <em>conformance</em> (stayed within authority), and <em>completeness</em> (all applicable controls enforced &amp; recorded). <em>Regulatory</em> reports platform anchoring maturity separately and is advisory until the external-KMS roadmap lands.</span>
              </li>
              <li className="flex gap-2">
                <span aria-hidden>②</span>
                <span><strong className="text-foreground">Inclusion log</strong> — the position number proves the receipt is chained into history. Removing or altering one breaks the chain.</span>
              </li>
              <li className="flex gap-2">
                <span aria-hidden>③</span>
                <span><strong className="text-foreground">Verify yourself</strong> — hit <em>Verify now</em> on any row. The backend re-checks the Ed25519 signature, the trust-envelope binding, and the signing key against the cluster&apos;s published anchor, live — no tooling to install, and no trust in this screen required.</span>
              </li>
              <li className="flex gap-2">
                <span aria-hidden>④</span>
                <span><strong className="text-foreground">Why a receipt reads &ldquo;Partial&rdquo;</strong> — one or more required per-run evidence axes were not bound when that receipt was signed, such as a retained eBPF datapath verdict for a sandbox that has already been retired. The detail names the missing axis. The regulatory/KMS roadmap disclosure does <em>not</em> by itself prevent a &ldquo;Verified&rdquo; verdict.</span>
              </li>
            </ul>
          </Section>
        </>
      )}
    </div>
  );
}
