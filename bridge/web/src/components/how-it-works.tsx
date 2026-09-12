import Link from "next/link";

/// The three-beat "how kars Bridge works" explainer. Extracted so it can be
/// shown BOTH in the first-run empty state AND via a persistent "How it works"
/// disclosure in the hero — so a returning user (who has missions/teams, and so
/// never hits the empty state) can still re-orient at any time (audit f1).
const STEPS = [
  { n: 1, t: "Describe the outcome", d: "Type what you want done. No config — plain language. Bridge picks the loop and composes an agent (or a whole team) for it." },
  { n: 2, t: "Review the plan", d: "You see the exact model, tools, network reach, autonomy, and budget before anything runs — and can change any of it. Nothing acts until you launch." },
  { n: 3, t: "Get verifiable work", d: "It starts automatically, runs live, steers to you when it needs a decision, and delivers with a signed receipt you can independently verify." },
] as const;

export function HowItWorksSteps({ withExamples = false }: { withExamples?: boolean }) {
  return (
    <>
      <ol className="mt-4 grid gap-4 sm:grid-cols-3">
        {STEPS.map((s) => (
          <li key={s.n} className="rounded-xl border border-border bg-surface-muted/30 p-4">
            <span className="grid h-6 w-6 place-items-center rounded-full bg-signal/15 text-xs font-semibold text-signal">{s.n}</span>
            <p className="mt-2 text-sm font-medium">{s.t}</p>
            <p className="mt-1 text-xs leading-relaxed text-foreground-muted">{s.d}</p>
          </li>
        ))}
      </ol>
      {withExamples && (
        <div className="mt-4 flex flex-wrap gap-2">
          <span className="text-xs text-foreground-muted">Try:</span>
          {["Summarise the latest changes in the Azure/kars repo", "Draft a competitive brief on agent runtimes", "Watch our repo and report failing CI daily"].map((ex) => (
            <Link key={ex} href={`/workspace/new?intent=${encodeURIComponent(ex)}`} className="rounded-full border border-signal/30 bg-signal/5 px-3 py-1 text-xs text-signal hover:bg-signal/10">
              {ex}
            </Link>
          ))}
        </div>
      )}
    </>
  );
}
