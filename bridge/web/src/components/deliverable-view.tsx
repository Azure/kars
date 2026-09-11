// kars Bridge — smart deliverable renderer. Turns an agent's raw markdown output
// into a well-formatted, TYPED document: it classifies what the agent produced
// (report / recommendation / action plan / note), lifts a summary into a
// callout, and renders the body with rich, themed markdown components (tables,
// headings, links, code, task-lists, callouts) instead of raw browser defaults.
//
// Server-renderable: react-markdown + remark-gfm run fine in RSC.

import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Children, isValidElement } from "react";
import type { Components } from "react-markdown";
import { Icon, type IconName } from "@/components/icon";
import { MermaidDiagram } from "@/components/mermaid-diagram";

export type DeliverableKind = "report" | "recommendation" | "action" | "note";

const KIND_META: Record<
  DeliverableKind,
  { icon: IconName; label: string; hint: string; accent: string }
> = {
  report: {
    icon: "chart",
    label: "Report",
    hint: "A structured briefing with findings and evidence.",
    accent: "border-signal/40 bg-signal/5",
  },
  recommendation: {
    icon: "lightbulb",
    label: "Recommendation",
    hint: "The agent's assessment and what it advises.",
    accent: "border-violet-500/40 bg-violet-500/5",
  },
  action: {
    icon: "check",
    label: "Action plan",
    hint: "Concrete next steps the agent proposes.",
    accent: "border-emerald-500/40 bg-emerald-500/5",
  },
  note: {
    icon: "note",
    label: "Note",
    hint: "The agent's written output.",
    accent: "border-border bg-surface",
  },
};

/** Strip glyphs that can't survive the plaintext transport (emoji → `?`), which
 * otherwise litter headings and table headers with orphan "?" marks. Removes a
 * leading "? " after markdown heading/list/table markers and collapses a bare
 * "?" cell. Purely cosmetic — never touches real content words. */
export function cleanDeliverable(raw: string): string {
  return raw
    // "## ? Title" / "### ? ? Title" → strip leading icon glyph(s)
    .replace(/^(#{1,6}\s+)(?:\?\s+)+/gm, "$1")
    // "- ? item" / "* ? item" → "- item"
    .replace(/^(\s*[-*]\s+)(?:\?\s+)+(?=\S)/gm, "$1")
    // table header/body cell that leads with "?" (mangled emoji column icon)
    .replace(/\|\s*\?\s+(?=\S)/g, "| ")
    // "**P0:** ? text" — icon glyph right after a bold lead-in
    .replace(/(\*\*[^*\n]{1,40}\*\*)\s+\?\s+/g, "$1 ")
    // "11? /" — icon glyph fused to a metric number, before whitespace/pipe
    .replace(/(\d)\s*\?(?=[\s|])/g, "$1")
    // "**? Do-not:**" — icon glyph fused inside the opening of a bold span
    .replace(/(\*\*)\?\s+/g, "$1")
    // standalone " ? " glyph between tokens: real punctuation attaches to the
    // preceding word ("done?"), so a space-isolated "?" is a mangled emoji.
    .replace(/ \?(?=[\s|)])/g, "")
    .replace(/[ \t]{2,}/g, " ")
    .replace(/[ \t]+([.,;:])/g, "$1")
    .trim();
}

/** Classify what the agent produced from lightweight textual signals. Order
 * matters: an explicit action list wins over a report, a recommendation over a
 * plain note. Conservative — defaults to "note" when nothing clearly matches. */
export function classifyDeliverable(raw: string): DeliverableKind {
  const t = raw.toLowerCase();
  const checkboxes = (raw.match(/^\s*[-*]\s+\[[ xX]\]/gm) ?? []).length;
  if (
    checkboxes >= 2 ||
    /^\s*#{1,6}\s+(action items|next steps|recommended actions|to ?do)\b/im.test(raw)
  ) {
    return "action";
  }
  if (
    /\b(i recommend|we recommend|recommendation:|my assessment|verdict:|bottom line:|in my opinion)\b/i.test(
      t,
    ) ||
    /^\s*#{1,6}\s+(recommendation|assessment|verdict|opinion)\b/im.test(raw)
  ) {
    return "recommendation";
  }
  const headings = (raw.match(/^#{1,6}\s+/gm) ?? []).length;
  const tables = (raw.match(/^\|.+\|\s*$/gm) ?? []).length;
  if (headings >= 2 || tables >= 2 || /executive summary|baseline|findings/i.test(t)) {
    return "report";
  }
  return "note";
}

/** Pull a short lead summary to surface as a callout, and report whether it came
 * from a named section (so the body can drop that section to avoid showing it
 * twice) vs. the first paragraph (which stays inline). */
export function extractSummary(raw: string): { text: string; fromSection: boolean } | null {
  const secMatch = raw.match(
    /^#{1,6}\s+(?:executive summary|summary|tl;?dr|overview)\s*\n+([\s\S]*?)(?=\n#{1,6}\s+|\n\s*\|)/im,
  );
  if (secMatch) {
    const s = secMatch[1].trim();
    if (s.length > 0) {
      return { text: s.length > 600 ? s.slice(0, 600).trimEnd() + "…" : s, fromSection: true };
    }
  }
  // First non-heading, non-table paragraph.
  const para = raw
    .split(/\n{2,}/)
    .map((p) => p.trim())
    .find((p) => p.length > 40 && !p.startsWith("#") && !p.startsWith("|"));
  if (para && para.length > 80) {
    return {
      text: para.length > 480 ? para.slice(0, 480).trimEnd() + "…" : para,
      fromSection: false,
    };
  }
  return null;
}

/** Remove the first "Executive summary"/"Summary"/"TL;DR"/"Overview" section
 * (heading + body up to the next heading/table) so it isn't shown twice when
 * it's already surfaced in the callout. */
export function stripLeadSummarySection(raw: string): string {
  return raw
    .replace(
      /^#{1,6}\s+(?:executive summary|summary|tl;?dr|overview)\s*\n+[\s\S]*?(?=\n#{1,6}\s+|\n\s*\|)/im,
      "",
    )
    .replace(/^\n+/, "")
    .trim();
}

/** Remove the first lead paragraph (the one [`extractSummary`] lifts when there
 * is no titled summary section) so a first-paragraph summary isn't rendered
 * twice — once in the callout and again at the top of the body. Matches the
 * same predicate `extractSummary` uses to pick that paragraph. */
export function stripLeadParagraph(raw: string): string {
  const blocks = raw.split(/\n{2,}/);
  const idx = blocks.findIndex((p) => {
    const t = p.trim();
    return t.length > 40 && !t.startsWith("#") && !t.startsWith("|");
  });
  if (idx === -1) return raw.trim();
  blocks.splice(idx, 1);
  return blocks.join("\n\n").replace(/^\n+/, "").trim();
}

const mdComponents: Components = {
  h1: ({ children }) => (
    <h1 className="mt-6 mb-3 border-b border-border pb-1.5 text-lg font-semibold tracking-tight first:mt-0">
      {children}
    </h1>
  ),
  h2: ({ children }) => (
    <h2 className="mt-6 mb-2 flex items-center gap-2 text-base font-semibold tracking-tight first:mt-0">
      <span className="h-3.5 w-1 rounded-full bg-signal/70" aria-hidden />
      {children}
    </h2>
  ),
  h3: ({ children }) => (
    <h3 className="mt-4 mb-1.5 text-sm font-semibold text-foreground first:mt-0">{children}</h3>
  ),
  h4: ({ children }) => (
    <h4 className="mt-3 mb-1 text-xs font-semibold uppercase tracking-wide text-foreground-muted first:mt-0">
      {children}
    </h4>
  ),
  p: ({ children }) => <p className="my-2 text-sm leading-relaxed text-foreground">{children}</p>,
  ul: ({ children }) => <ul className="my-2 space-y-1 pl-1 text-sm">{children}</ul>,
  ol: ({ children }) => (
    <ol className="my-2 list-decimal space-y-1 pl-5 text-sm marker:text-foreground-muted">
      {children}
    </ol>
  ),
  li: ({ children, className }) => {
    // remark-gfm task-list items carry `task-list-item`; render a clean checkbox.
    const isTask = typeof className === "string" && className.includes("task-list-item");
    if (isTask) {
      return <li className="flex list-none items-start gap-2 leading-relaxed">{children}</li>;
    }
    return (
      <li className="relative pl-4 leading-relaxed before:absolute before:left-0 before:top-[0.55em] before:h-1.5 before:w-1.5 before:rounded-full before:bg-signal/60">
        {children}
      </li>
    );
  },
  input: ({ checked }) => (
    <span
      className={`mt-0.5 inline-flex h-4 w-4 shrink-0 items-center justify-center rounded border text-[10px] ${
        checked
          ? "border-emerald-500/50 bg-emerald-500/15 text-emerald-600"
          : "border-border bg-surface text-transparent"
      }`}
      aria-hidden
    >
      {checked ? "✓" : ""}
    </span>
  ),
  a: ({ href, children }) => (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className="font-medium text-signal underline-offset-2 hover:underline"
    >
      {children}
      <span className="ml-0.5 text-[0.7em] opacity-60" aria-hidden>
        ↗
      </span>
    </a>
  ),
  strong: ({ children }) => <strong className="font-semibold text-foreground">{children}</strong>,
  blockquote: ({ children }) => (
    <blockquote className="my-3 rounded-r-md border-l-2 border-signal/50 bg-surface-muted/50 px-3 py-1.5 text-sm text-foreground-muted">
      {children}
    </blockquote>
  ),
  hr: () => <hr className="my-4 border-border" />,
  code: ({ className, children }) => {
    if (className === "language-mermaid") {
      return (
        <MermaidDiagram
          chart={String(children).replace(/\n$/, "")}
          className="language-mermaid"
        />
      );
    }
    const isBlock = typeof className === "string" && className.startsWith("language-");
    if (isBlock) {
      return (
        <code className={`${className} block`}>{children}</code>
      );
    }
    return (
      <code className="rounded bg-surface-muted px-1.5 py-0.5 font-mono text-[0.85em] text-foreground">
        {children}
      </code>
    );
  },
  pre: ({ children }) => {
    const child = Children.count(children) === 1 ? Children.only(children) : null;
    if (
      isValidElement<{ className?: string }>(child)
      && child.props.className === "language-mermaid"
    ) {
      return child;
    }
    return (
      <pre className="my-3 overflow-x-auto rounded-lg border border-border bg-surface-muted p-3 font-mono text-xs leading-relaxed">
        {children}
      </pre>
    );
  },
  table: ({ children }) => (
    <div className="my-3 overflow-x-auto rounded-lg border border-border">
      <table className="w-full border-collapse text-sm">{children}</table>
    </div>
  ),
  thead: ({ children }) => <thead className="bg-surface-muted">{children}</thead>,
  th: ({ children }) => (
    <th className="border-b border-border px-3 py-2 text-left text-xs font-semibold text-foreground-muted">
      {children}
    </th>
  ),
  td: ({ children }) => (
    <td className="border-b border-border/60 px-3 py-2 align-top text-foreground">{children}</td>
  ),
};

/** Reduce agent markdown to a clean one-line-ish plain preview: strip heading
 * markers, emphasis, links (keep text), table pipes, and mangled-emoji "?".
 * For compact previews (commons entries, cards) where full markdown would be
 * noise in a clamped box. */
export function toPlainPreview(raw: string): string {
  return cleanDeliverable(raw)
    .replace(/^#{1,6}\s+/gm, "") // heading markers
    .replace(/\*\*([^*]+)\*\*/g, "$1") // bold
    .replace(/\*([^*]+)\*/g, "$1") // italic
    .replace(/`([^`]+)`/g, "$1") // inline code
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1") // links → text
    .replace(/^\s*[-*]\s+/gm, "• ") // list markers → bullet
    .replace(/^\s*\|.*\|\s*$/gm, "") // drop table rows
    .replace(/^\s*[-:| ]+\s*$/gm, "") // drop table separators
    .replace(/\n{2,}/g, " · ") // paragraph breaks → separator
    .replace(/\s+/g, " ")
    .trim();
}

export function DeliverableBody({ output }: { output: string }) {
  const clean = cleanDeliverable(output);
  return (
    <div className="text-sm">
      <Markdown remarkPlugins={[remarkGfm]} components={mdComponents}>
        {clean}
      </Markdown>
    </div>
  );
}

export function DeliverableView({
  output,
  model,
  totalTokens,
  finishedAt,
  artifactCount,
}: {
  output: string;
  model?: string | null;
  totalTokens?: number | null;
  finishedAt?: string | null;
  artifactCount?: number | null;
}) {
  const clean = cleanDeliverable(output);
  const kind = classifyDeliverable(clean);
  const meta = KIND_META[kind];
  const summary = extractSummary(clean);
  const body = summary
    ? summary.fromSection
      ? stripLeadSummarySection(clean)
      : stripLeadParagraph(clean)
    : clean;

  return (
    <section className={`overflow-hidden rounded-xl border ${meta.accent}`}>
      {/* Header */}
      <div className="flex flex-wrap items-start justify-between gap-3 border-b border-border/60 px-5 py-4">
        <div className="flex items-start gap-3">
          <span className="text-xl leading-none" aria-hidden>
            <Icon name={meta.icon} size={22} />
          </span>
          <div>
            <div className="flex items-center gap-2">
              <h2 className="text-sm font-semibold">Deliverable</h2>
              <span className="rounded-full border border-border bg-surface px-2 py-0.5 text-[10px] font-medium text-foreground-muted">
                {meta.label}
              </span>
            </div>
            <p className="mt-0.5 text-xs text-foreground-muted">{meta.hint}</p>
          </div>
        </div>
        <dl className="flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-foreground-muted">
          {model && (
            <div className="flex items-center gap-1">
              <dt><Icon name="brain" size={13} /></dt>
              <dd className="font-medium text-foreground">{model}</dd>
            </div>
          )}
          {totalTokens != null && (
            <div className="flex items-center gap-1">
              <dt>tokens</dt>
              <dd className="font-medium text-foreground">{totalTokens.toLocaleString()}</dd>
            </div>
          )}
          {artifactCount != null && artifactCount > 0 && (
            <div className="flex items-center gap-1">
              <dt>files</dt>
              <dd className="font-medium text-foreground">{artifactCount}</dd>
            </div>
          )}
          {finishedAt && (
            <div className="flex items-center gap-1">
              <dt>produced</dt>
              <dd className="font-medium text-foreground">
                {new Date(finishedAt).toLocaleString()}
              </dd>
            </div>
          )}
        </dl>
      </div>

      {/* Summary callout */}
      {summary && (
        <div className="border-b border-border/60 bg-surface/60 px-5 py-3">
          <p className="text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
            {kind === "recommendation" ? "The gist" : "In brief"}
          </p>
          <div className="mt-1">
            <DeliverableBody output={summary.text} />
          </div>
        </div>
      )}

      {/* Full body */}
      <div className="max-h-[32rem] overflow-auto bg-surface px-5 py-4">
        <DeliverableBody output={body} />
      </div>
    </section>
  );
}
