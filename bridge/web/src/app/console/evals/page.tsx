// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge Operator Console — Safety evals (KarsEval).
//
// A failed expectation need not be an allowed attack: policy attribution,
// reason matching, or required observations can fail for a blocked request.

import { PageHeader, Stat, Badge } from "@/components/ui";
import { HonestState } from "@/components/honest-state";
import { listEvals, listSandboxes } from "@/lib/bff";
import type { Eval } from "@/lib/types";
import { NewEvalForm } from "./new-eval-form";
import { EvalDetail } from "./eval-detail";

export const dynamic = "force-dynamic";

function phaseTone(phase: string | null): "ok" | "warn" | "danger" | "muted" {
  if (phase === "Ready") return "ok";
  if (phase === "Degraded" || phase === "Failed") return "danger";
  if (phase === "Pending" || phase === "Progressing") return "warn";
  return "muted";
}

function ago(iso: string | null): string {
  if (!iso) return "never";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const s = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

function EvalCard({ e }: { e: Eval }) {
  const r = e.last_result;
  const passRate = r && r.total > 0 ? Math.round((r.passed / r.total) * 100) : null;
  const anyFailed = r != null && r.failed > 0;
  const allPassed = r != null && r.total > 0 && r.passed === r.total && r.failed === 0 && r.errored === 0;
  return (
    <li className="rounded-xl border border-border bg-surface p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="font-medium">{e.display_name ?? e.name}</p>
          <p className="mt-0.5 text-xs text-foreground-muted">
            {e.corpus ?? "—"}
            {e.target_sandbox && <> · target <span className="font-mono">{e.target_sandbox}</span></>}
            {e.schedule && <> · schedule <span className="font-mono">{e.schedule}</span></>}
          </p>
        </div>
        <Badge tone={phaseTone(e.phase)} dot>
          {e.phase ?? "Unknown"}
        </Badge>
      </div>

      {r ? (
        <>
          <div className="mt-4 grid grid-cols-4 gap-3">
            <div>
              <p className="text-lg font-semibold tabular-nums">{r.total}</p>
              <p className="text-[11px] text-foreground-muted">cases</p>
            </div>
            <div>
              <p className="text-lg font-semibold tabular-nums text-ok">{r.passed}</p>
              <p className="text-[11px] text-foreground-muted">passed</p>
            </div>
            <div>
              <p className={`text-lg font-semibold tabular-nums ${r.failed > 0 ? "text-danger" : ""}`}>{r.failed}</p>
              <p className="text-[11px] text-foreground-muted">failed</p>
            </div>
            <div>
              <p className={`text-lg font-semibold tabular-nums ${r.errored > 0 ? "text-warning" : ""}`}>{r.errored}</p>
              <p className="text-[11px] text-foreground-muted">errored</p>
            </div>
          </div>
          {passRate != null && (
            <div className="mt-3 h-2 w-full overflow-hidden rounded-full bg-surface-muted">
              <div className={`h-full ${anyFailed ? "bg-danger" : allPassed ? "bg-ok" : "bg-warning"}`} style={{ width: `${Math.min(100, Math.max(passRate, 0))}%` }} />
            </div>
          )}
          <p className="mt-2 text-[11px] text-foreground-muted">
            {anyFailed
              ? `${r.failed} case${r.failed === 1 ? "" : "s"} did not match ${r.failed === 1 ? "its" : "their"} declared expectations. Inspect the case evidence to determine why.`
              : r.total === 0
                ? "No cases were evaluated; no safety verdict is available."
                : allPassed
                  ? "Every evaluated case matched its declared expectations."
                  : "The run is inconclusive; not every case has a passing verdict."}
            {r.errored > 0 && (
              <>
                {" "}
                {r.errored} case{r.errored === 1 ? "" : "s"}
                {" couldn\u2019t be evaluated — inspect the case evidence before re-running; errors are not passing verdicts."}
              </>
            )}{" "}
            Last run {ago(e.last_run_at)}.
          </p>
          <EvalDetail name={e.name} />
        </>
      ) : (
        <>
          <p className="mt-4 text-xs text-foreground-muted">
            No completed run yet — the corpus replay is pending or in flight.
          </p>
          <EvalDetail name={e.name} />
        </>
      )}
    </li>
  );
}

export default async function EvalsPage() {
  const [evalResult, sandboxResult] = await Promise.allSettled([listEvals(), listSandboxes()]);
  const error = evalResult.status === "rejected";
  const sandboxError = sandboxResult.status === "rejected";
  const evals: Eval[] = evalResult.status === "fulfilled" ? evalResult.value : [];
  const sandboxes = sandboxResult.status === "fulfilled" ? sandboxResult.value
      .filter((s) => s.phase === "Running" || s.phase === "Ready")
      .map((s) => ({ name: s.name, runtime: s.runtime })) : [];

  const withResult = evals.filter((e) => e.last_result);
  const drifting = withResult.filter((e) => (e.last_result?.failed ?? 0) > 0).length;
  const cases = withResult.reduce((s, e) => s + (e.last_result?.total ?? 0), 0);

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="Safety evals"
        lead="Each KarsEval replays curated jailbreak, injection, or egress probes against a live sandbox's inference router. Failures can reflect a mismatched decision, policy attribution, or reason, or a missing required observation — not necessarily a successful attack. Inspect the detailed report for the evidence."
      />

      {sandboxError && (
        <HonestState variant="not_wired" title="Sandbox inventory unavailable"
          detail="Running targets could not be read. New evals are unavailable until inventory recovers; existing reports remain below." />
      )}

      {!error && !sandboxError && (
        <div className="flex items-center justify-between gap-3">
          <div />
          <NewEvalForm sandboxes={sandboxes} />
        </div>
      )}

      {!error && evals.length > 0 && (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Stat label="Evals" value={evals.length} />
          <Stat label="With reports" value={withResult.length} />
          <Stat label="Drifting" value={drifting} accent={drifting > 0} />
          <Stat label="Cases replayed" value={cases} />
        </div>
      )}

      {error ? (
        <HonestState variant="not_wired" title="Evaluation history unavailable" detail="The evaluation records could not be read. No empty history or safety verdict is asserted." />
      ) : evals.length === 0 ? (
        <HonestState
          variant="empty"
          title="No safety evals yet"
          detail="Create a KarsEval targeting a sandbox with a builtin corpus (e.g. jailbreak-baseline) — its real verdict appears here once the runner completes."
        />
      ) : (
        <ul className="space-y-3">
          {evals.map((e) => (
            <EvalCard key={`${e.namespace}/${e.name}`} e={e} />
          ))}
        </ul>
      )}
    </div>
  );
}
