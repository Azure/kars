"use client";

// Downloadable audit report. Compiles the REAL governance proofs already on
// this page — the signed receipt (envelope digest, signatures, claims,
// transparency-log inclusion, the exact independent verify command) plus the
// execution trace summary and enforced egress — into a human-readable Markdown
// report and the raw JSON evidence bundle, and lets the operator download both.
// Nothing is synthesized: every line traces to a real receipt/trace field.

import type { ActivityEvent, Receipt } from "@/lib/types";

function download(filename: string, content: string, mime: string) {
  const blob = new Blob([content], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

function buildMarkdown(task: string, receipt: Receipt, activity: ActivityEvent[], egress: string[]): string {
  const tools = activity.filter((e) => e.kind === "tool");
  const rounds = activity.filter((e) => e.kind === "round");
  const failed = tools.filter((e) => e.kind === "tool" && !e.ok).length;
  const totalTokens = rounds.reduce((s, e) => s + (e.kind === "round" ? e.total_tokens : 0), 0);
  const lines: string[] = [];
  lines.push(`# Audit report — ${task}`);
  lines.push("");
  lines.push(`_Generated ${new Date().toISOString()} from the mission's signed Governance Receipt and execution trace._`);
  lines.push("");
  lines.push("## Attestation");
  lines.push(`- **Receipt**: \`${receipt.name}\` (namespace \`${receipt.namespace}\`)`);
  lines.push(`- **Predicate**: ${receipt.predicate_type}`);
  lines.push(`- **Envelope digest**: \`${receipt.envelope_digest}\``);
  lines.push(`- **Issued**: ${receipt.issued_at ?? "—"}`);
  lines.push(`- **Signature scheme**: ${receipt.scheme} (key \`${receipt.key_id}\`)`);
  lines.push(`- **Signatures**: ${receipt.signatures.length}`);
  if (receipt.inclusion_seq !== null) {
    lines.push(`- **Transparency log**: entry #${receipt.inclusion_seq}, hash \`${receipt.inclusion_entry_hash ?? "—"}\``);
  }
  if (receipt.checkpoint) {
    lines.push(`- **Signed checkpoint (STH)**: present — cross-receipt tamper-evidence chain`);
  }
  lines.push("");
  lines.push("## Claims");
  if (receipt.claims.length === 0) {
    lines.push("_No claims recorded._");
  } else {
    for (const c of receipt.claims) {
      lines.push(`- **${c.class}** — ${c.status}: ${c.detail}`);
    }
  }
  lines.push("");
  lines.push("## Enforced egress");
  lines.push(egress.length === 0 ? "- Model path only — all other egress denied at the boundary." : egress.map((h) => `- \`${h}\``).join("\n"));
  lines.push("");
  lines.push("## Execution trace summary");
  lines.push(`- Model rounds: ${rounds.length}`);
  lines.push(`- Tool calls: ${tools.length} (${failed} failed)`);
  lines.push(`- Tokens observed in trace: ${totalTokens.toLocaleString()}`);
  lines.push(`  _(agent-side per-round count from the execution trace; the billed total on the mission scorecard may differ — it includes system-prompt and tool-call overhead the per-round trace doesn't.)_`);
  const toolNames = [...new Set(tools.map((e) => (e.kind === "tool" ? e.name : "")))].filter(Boolean);
  if (toolNames.length) lines.push(`- Tools used: ${toolNames.join(", ")}`);
  lines.push("");
  lines.push("## Independent verification");
  lines.push("Anyone can verify this receipt's signature without trusting the Bridge:");
  lines.push("");
  lines.push("```");
  lines.push(receipt.verify_command || "(verify command unavailable)");
  lines.push("```");
  lines.push("");
  return lines.join("\n");
}

export function AuditReportDownload({
  task,
  receipt,
  activity,
  egress,
}: {
  task: string;
  receipt: Receipt;
  activity: ActivityEvent[];
  egress: string[];
}) {
  const stamp = new Date().toISOString().slice(0, 10);
  return (
    <div className="inline-flex gap-2">
      <button
        type="button"
        onClick={() => download(`audit-${task}-${stamp}.md`, buildMarkdown(task, receipt, activity, egress), "text/markdown")}
        className="inline-flex items-center gap-1.5 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium hover:bg-surface-muted"
        title="Human-readable Markdown report of the signed proofs"
      >
        ⬇ Download report
      </button>
      <button
        type="button"
        onClick={() =>
          download(
            `audit-${task}-${stamp}.json`,
            JSON.stringify({ receipt, activity, egress, generated_at: new Date().toISOString() }, null, 2),
            "application/json",
          )
        }
        className="inline-flex items-center gap-1.5 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium text-foreground-muted hover:bg-surface-muted"
        title="Raw evidence bundle (receipt + trace + egress) as JSON"
      >
        ⬇ Evidence JSON
      </button>
    </div>
  );
}
