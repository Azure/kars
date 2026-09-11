"use client";

// kars Bridge — Provenance Story. The detailed, plain-language answer to "how
// did this get made?" built from the REAL trace: model rounds, every tool call
// with its actual parameters + result + duration, files touched, and any
// network destinations. Foldable per step. Nothing is inferred — each row is a
// recorded event. Shared by mission, artifact, and agent views.

import { useState } from "react";
import type { ActivityEvent } from "@/lib/types";
import { Icon, type IconName } from "@/components/icon";

type ToolEvent = Extract<ActivityEvent, { kind: "tool" }>;

function classify(name: string): { icon: IconName; verb: string; cat: string } {
  const n = name.toLowerCase();
  if (/(write|create|save|edit|patch|append)/.test(n)) return { icon: "pencil", verb: "wrote a file", cat: "file" };
  if (/(read|cat|open|view|list|glob|grep|find)/.test(n)) return { icon: "file", verb: "read files", cat: "file" };
  if (/(git|commit|push|pull|pr|branch)/.test(n)) return { icon: "branch", verb: "ran git", cat: "git" };
  if (/(http|fetch|web|search|browse|curl|api|crawl|tavily|brave)/.test(n)) return { icon: "globe", verb: "reached the network", cat: "net" };
  if (/(shell|bash|exec|run|command)/.test(n)) return { icon: "terminal", verb: "ran a command", cat: "exec" };
  return { icon: "wrench", verb: "used a tool", cat: "tool" };
}

function pathFrom(t: ToolEvent): string | null {
  const m = t.result_preview?.match(/\/[\w./-]+\.\w+/);
  return m ? m[0].split("/").pop()! : null;
}
function domainFrom(t: ToolEvent): string | null {
  const m = (t.args_preview ?? "").match(/https?:\/\/([\w.-]+)/);
  return m ? m[1] : null;
}

export function ProvenanceStory({ activity, egress, artifactName, tokens }: { activity: ActivityEvent[]; egress?: string[]; artifactName?: string; tokens?: number | null }) {
  const tools = activity.filter((e): e is ToolEvent => e.kind === "tool");
  const rounds = activity.filter((e) => e.kind === "round").length;
  const cats = tools.reduce<Record<string, number>>((m, t) => { const c = classify(t.name).cat; m[c] = (m[c] ?? 0) + 1; return m; }, {});
  const domains = Array.from(new Set([...(tools.map(domainFrom).filter(Boolean) as string[]), ...(egress ?? [])]));
  const summary = [rounds && `${rounds} rounds`, cats.file && `${cats.file} file ops`, cats.net && `${cats.net} network`, cats.git && `${cats.git} git`, cats.exec && `${cats.exec} commands`, tokens ? `${tokens.toLocaleString()} tok` : null].filter(Boolean).join(" · ");

  if (activity.length === 0) return <p className="text-xs text-foreground-muted">No execution trace yet — run it to record how the deliverable is produced.</p>;

  const numberedActivity: Array<{ event: ActivityEvent; roundOrdinal: number }> = [];
  let roundOrdinal = 0;
  for (const event of activity) {
    if (event.kind === "round") roundOrdinal += 1;
    numberedActivity.push({ event, roundOrdinal });
  }

  return (
    <div className="space-y-3">
      <p className="text-sm">{artifactName ? <><span className="font-mono text-xs">{artifactName}</span> via </> : "Produced via "}<strong>{summary}</strong>. Every step is a recorded action.</p>
      {domains.length > 0 && (
        <p className="text-xs text-foreground-muted">Reached: {domains.map((h) => <span key={h} className="mr-1 inline-block rounded bg-surface-muted px-1.5 py-0.5 font-mono">{h}</span>)} — all other egress denied.</p>
      )}
      <ol className="space-y-1 border-l border-border pl-3">
        {numberedActivity.map(({ event: e, roundOrdinal }, i) => {
          if (e.kind === "round") {
            return (
              <li key={i} className="flex items-center gap-2 py-0.5 text-xs">
                <Icon name="brain" className="text-foreground-muted" />
                <span className="font-medium">Round {roundOrdinal}</span>
                <span className="text-foreground-muted">{e.tool_calls > 0 ? `chose ${e.tool_calls} tool` : "reasoned"} · {e.total_tokens.toLocaleString()} tok · {e.ms}ms</span>
              </li>
            );
          }
          return <ToolRow key={i} t={e} />;
        })}
      </ol>
    </div>
  );
}

function ToolRow({ t }: { t: ToolEvent }) {
  const [open, setOpen] = useState(false);
  const c = classify(t.name);
  const file = pathFrom(t);
  return (
    <li className="text-xs">
      <button type="button" onClick={() => setOpen(!open)} className="flex w-full items-center gap-2 py-0.5 text-left hover:text-foreground">
        <Icon name={c.icon} className="text-foreground-muted" />
        <span>{c.verb}</span>
        <span className="font-mono text-foreground-muted">{t.name}{file ? ` · ${file}` : ""}</span>
        {!t.ok && <span className="text-danger">failed</span>}
        <span className="ml-auto inline-flex items-center gap-1 text-foreground-muted">{t.ms}ms <Icon name="chevron-down" size={12} className={`transition-transform ${open ? "rotate-180" : ""}`} /></span>
      </button>
      {open && (
        <div className="mb-1 ml-6 space-y-1 rounded border border-border bg-surface-muted/40 p-2 font-mono text-[11px]">
          <div className="break-words"><span className="text-foreground-muted">params:</span> {t.args_preview || "—"}</div>
          <div className="break-words"><span className="text-foreground-muted">result:</span> {t.result_preview || "—"}</div>
        </div>
      )}
    </li>
  );
}
