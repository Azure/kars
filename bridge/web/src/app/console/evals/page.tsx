// kars Bridge Operator Console — Safety evals (KarsEval).
//
// The quality/safety lifecycle: each KarsEval replays a curated adversarial
// corpus (jailbreak / injection / egress) against a live sandbox's inference
// router and records a real pass/fail verdict. This page surfaces the REAL
// KarsEval status the controller captured — never fabricated. A failing eval
// means the sandbox let an attack through (drift), which is exactly the signal
// an operator needs. Honest empty when no evals exist.

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
              <div className={`h-full ${anyFailed ? "bg-danger" : "bg-ok"}`} style={{ width: `${Math.max(passRate, 2)}%` }} />
            </div>
          )}
          <p className="mt-2 text-[11px] text-foreground-muted">
            {anyFailed
              ? `${r.failed} attack${r.failed === 1 ? " was" : "s were"} NOT blocked — the sandbox drifted from its safety baseline.`
              : r.errored > 0
                ? "No safety failures detected."
                : "Every adversarial case was correctly handled."}
            {r.errored > 0 && (
              <>
                {" "}
                {r.errored} case{r.errored === 1 ? "" : "s"}
                {" couldn\u2019t be evaluated (target unreachable) — inconclusive, not counted as a failure; re-run against a live sandbox."}
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
  let evals: Eval[] = [];
  let sandboxes: { name: string; runtime: string | null }[] = [];
  let error = false;
  try {
    const [ev, sb] = await Promise.all([listEvals(), listSandboxes().catch(() => [])]);
    evals = ev;
    sandboxes = sb
      .filter((s) => s.phase === "Running" || s.phase === "Ready")
      .map((s) => ({ name: s.name, runtime: s.runtime }));
  } catch {
    error = true;
  }

  const withResult = evals.filter((e) => e.last_result);
  const drifting = withResult.filter((e) => (e.last_result?.failed ?? 0) > 0).length;
  const cases = withResult.reduce((s, e) => s + (e.last_result?.total ?? 0), 0);

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="Safety evals"
        lead="Each KarsEval replays a curated adversarial corpus (jailbreak / injection / egress) against a live sandbox's inference router and records a real pass/fail verdict. A failing case means an attack got through — the drift signal that matters."
      />

      {!error && (
        <div className="flex items-center justify-between gap-3">
          <div />
          <NewEvalForm sandboxes={sandboxes} />
        </div>
      )}

      {!error && evals.length > 0 && (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          <Stat label="Evals" value={evals.length} />
          <Stat label="With a verdict" value={withResult.length} />
          <Stat label="Drifting" value={drifting} accent={drifting > 0} />
          <Stat label="Cases replayed" value={cases} />
        </div>
      )}

      {error ? (
        <HonestState variant="not_wired" title="Cluster unreachable" detail="The Bridge backend can't reach the Kubernetes API right now." />
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
