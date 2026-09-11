"use client";

import { useEffect, useId, useRef, useState } from "react";

let initialized = false;

function isFlowchartSource(chart: string): boolean {
  for (const line of chart.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("%%")) continue;
    return /^(?:flowchart|graph)\b/i.test(trimmed);
  }
  return false;
}

function normalizeFlowchartLabels(chart: string): string {
  if (!isFlowchartSource(chart)) return chart;

  let changed = false;
  const normalized = chart.replace(
    /(^|[^[(])\[(?![\[(])([^\]\n]*?)\]/gm,
    (match, prefix: string, label: string) => {
      if (!/[()]/.test(label)) return match;
      const trimmed = label.trimStart();
      if (!trimmed || /^["'`/\\>]/.test(trimmed)) return match;
      changed = true;
      return `${prefix}[${JSON.stringify(label)}]`;
    },
  );

  return changed ? normalized : chart;
}

async function preflightMermaidSource(
  mermaid: typeof import("mermaid").default,
  chart: string,
): Promise<string> {
  try {
    await mermaid.parse(chart);
    return chart;
  } catch (rawError) {
    const normalized = normalizeFlowchartLabels(chart);
    if (normalized !== chart) {
      try {
        await mermaid.parse(normalized);
        return normalized;
      } catch {
        // Fall through to the original parse error so the fallback keeps the
        // readable source and a single concise error message.
      }
    }
    throw rawError;
  }
}

export function MermaidDiagram({ chart }: { chart: string; className?: string }) {
  const id = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const target = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void import("mermaid")
      .then(async ({ default: mermaid }) => {
        if (!initialized) {
          mermaid.initialize({
            startOnLoad: false,
            securityLevel: "strict",
            theme: "neutral",
            fontFamily: "ui-sans-serif, system-ui, sans-serif",
          });
          initialized = true;
        }
        const source = await preflightMermaidSource(mermaid, chart);
        const rendered = await mermaid.render(`kars-mermaid-${id}`, source);
        if (cancelled || !target.current) return;
        target.current.innerHTML = rendered.svg;
        rendered.bindFunctions?.(target.current);
        setError(null);
      })
      .catch((reason: unknown) => {
        if (cancelled) return;
        setError(reason instanceof Error ? reason.message : "Diagram rendering failed");
      });
    return () => {
      cancelled = true;
    };
  }, [chart, id]);

  if (error) {
    return (
      <figure className="my-4 overflow-hidden rounded-xl border border-warning/30 bg-warning/5">
        <figcaption className="border-b border-warning/20 px-3 py-2 text-xs text-warning">
          Mermaid could not render this diagram: {error}
        </figcaption>
        <pre className="overflow-x-auto p-3 font-mono text-xs leading-relaxed">{chart}</pre>
      </figure>
    );
  }

  return (
    <figure className="my-4 overflow-x-auto rounded-xl border border-border bg-white p-4">
      <div
        ref={target}
        role="img"
        aria-label="Rendered Mermaid diagram"
        className="min-w-fit [&_svg]:mx-auto [&_svg]:h-auto [&_svg]:max-w-full"
      />
    </figure>
  );
}
