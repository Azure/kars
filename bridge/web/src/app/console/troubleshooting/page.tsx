// kars Bridge — System / wiring view. Delivery Constraint #5: the product
// never hides an un-wired gap behind a finished screen. This page shows the
// true, cluster-read status of every stage of the governed-agent pipeline.

import { PageHeader, Section, Stat } from "@/components/ui";
import { WiringBadge } from "@/components/wiring-badge";
import { Icon } from "@/components/icon";
import { authWired, operatorIdentity } from "@/lib/config";
import { BffError, getSystem, getDiagnostics } from "@/lib/bff";
import type { WiringStatus, Diagnostics } from "@/lib/types";

export const dynamic = "force-dynamic";

function connectorTone(status: WiringStatus): string {
  return status === "live" ? "bg-ok/40" : "bg-border";
}

const LEAD =
  "Real problems first — what's failing right now and how to fix it — then the honest end-to-end wiring map. Every status here is read from the live cluster, nothing implied to work that doesn't.";

export default async function SystemPage() {
  let system;
  try {
    system = await getSystem();
  } catch (err) {
    const code = err instanceof BffError ? err.code : "unknown";
    return (
      <div className="space-y-6">
        <PageHeader eyebrow="Operator Console" title="Troubleshooting" lead={LEAD} />
        <div className="kb-card p-8 text-center text-sm text-foreground-muted">
          {code === "cluster_unavailable"
            ? "Not connected to a cluster — system status is unavailable."
            : `Could not load system status (${code}).`}
        </div>
      </div>
    );
  }

  let diagnostics: Diagnostics | null = null;
  try {
    diagnostics = await getDiagnostics();
  } catch {
    diagnostics = null;
  }

  const liveCount = system.pipeline.filter((s) => s.status === "live").length;
  const crdsInstalled = system.crds.filter((c) => c.installed).length;
  const critical = diagnostics?.issues.filter((i) => i.severity === "critical").length ?? 0;

  return (
    <div className="space-y-6">
      <PageHeader eyebrow="Operator Console" title="Troubleshooting" lead={LEAD} />

      {!authWired() && (
        <div className="rounded-xl border border-warn/40 bg-warn/10 p-4">
          <p className="inline-flex items-center gap-2 text-sm font-semibold text-foreground">
            <Icon name="shield" className="h-4 w-4 text-warn" aria-hidden />
            Identity disclosure — decisions are attributed to a configured operator
          </p>
          <p className="mt-1 text-xs text-foreground-muted">
            This Bridge does not yet have per-user sign-in wired. Every approval, denial,
            and launch made through the UI is recorded as <span className="font-mono">{operatorIdentity()}</span>,
            not a logged-in individual. Real per-user authentication (binding the decider
            to an authenticated session) is a named next step — surfaced here, and next to
            every decision control, so nothing is implied that can&rsquo;t be proven.
          </p>
        </div>
      )}

      {/* Active issues — the real diagnostics: what's broken, and the fix. */}
      {diagnostics && (
        <Section
          title="Active issues"
          subtitle={
            diagnostics.healthy
              ? `All clear — scanned ${diagnostics.scanned_pods} pod${diagnostics.scanned_pods === 1 ? "" : "s"} and ${diagnostics.scanned_sandboxes} sandbox${diagnostics.scanned_sandboxes === 1 ? "" : "es"}, nothing failing.`
              : `${diagnostics.issues.length} issue${diagnostics.issues.length === 1 ? "" : "s"}${critical > 0 ? ` · ${critical} critical` : ""} across ${diagnostics.scanned_pods} pods.`
          }
        >
          {diagnostics.healthy ? (
            <div className="flex items-center gap-3 rounded-xl border border-ok/30 bg-ok/[0.05] p-4">
              <span aria-hidden className="grid h-9 w-9 place-items-center rounded-lg bg-surface text-lg"><Icon name="check" size={20} /></span>
              <p className="text-sm text-foreground">No failing pods, crash loops, image-pull errors, or degraded sandboxes right now.</p>
            </div>
          ) : (
            <ul className="space-y-2">
              {diagnostics.issues.map((issue, i) => {
                const crit = issue.severity === "critical";
                return (
                  <li
                    key={`${issue.subject}-${issue.kind}-${i}`}
                    className={`rounded-xl border p-4 ${crit ? "border-rose-500/30 bg-rose-500/[0.04]" : "border-amber-500/30 bg-amber-500/[0.04]"}`}
                  >
                    <div className="flex flex-wrap items-center gap-2">
                      <span
                        className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${crit ? "bg-rose-500/15 text-rose-600" : "bg-amber-500/15 text-amber-600"}`}
                      >
                        {crit ? "Critical" : "Warning"}
                      </span>
                      <code className="font-mono text-xs font-semibold text-foreground">{issue.kind}</code>
                      <span className="text-xs text-foreground-muted">·</span>
                      <code className="font-mono text-[11px] text-foreground-muted">{issue.subject}</code>
                      <span className="ml-auto text-[11px] text-foreground-muted">{issue.reason}</span>
                    </div>
                    {issue.detail && (
                      <p className="mt-1.5 font-mono text-[11px] leading-relaxed text-foreground-muted">{issue.detail}</p>
                    )}
                    <p className="mt-1.5 text-xs text-foreground">
                      <span className="font-medium">Fix:</span> {issue.remedy}
                    </p>
                    <div className="mt-2 flex flex-wrap gap-3 text-[11px]">
                      <a
                        href={`/console/fleet?q=${encodeURIComponent(issue.subject.split("/").pop() ?? "")}`}
                        className="font-medium text-signal hover:underline"
                      >
                        Inspect sandbox in fleet →
                      </a>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </Section>
      )}

      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
        <Stat label="Pipeline stages live" value={`${liveCount}/${system.pipeline.length}`} accent={liveCount === system.pipeline.length} />
        <Stat label="CRDs installed" value={`${crdsInstalled}/${system.crds.length}`} />
        <div className="rounded-xl border border-border bg-surface p-4">
          <p className="inline-flex items-center gap-2 text-sm font-semibold">
            Controller
            <WiringBadge status={system.controller_reachable ? "live" : "not_wired"} />
          </p>
          <p className="mt-1 text-xs text-foreground-muted">The reconciler that materializes every sandbox.</p>
        </div>
      </div>

      <Section
        title="Pipeline wiring"
        subtitle={`${liveCount} of ${system.pipeline.length} stages are live end-to-end. The rest are named here so nothing is hidden.`}
      >
        <ol className="space-y-0">
          {system.pipeline.map((stage, i) => (
            <li key={stage.id} className="relative flex gap-4">
              <div className="flex flex-col items-center">
                <span
                  className={[
                    "grid h-7 w-7 shrink-0 place-items-center rounded-full border text-xs font-semibold",
                    stage.status === "live"
                      ? "border-ok/40 bg-ok/15 text-ok"
                      : stage.status === "partial"
                        ? "border-warning/40 bg-warning/15 text-warning"
                        : "border-border bg-surface-muted text-foreground-muted",
                  ].join(" ")}
                >
                  {i + 1}
                </span>
                {i < system.pipeline.length - 1 && (
                  <span className={`w-px flex-1 ${connectorTone(system.pipeline[i + 1].status)}`} style={{ minHeight: "1.5rem" }} />
                )}
              </div>
              <div className="flex-1 pb-6">
                <div className="flex flex-wrap items-center gap-2">
                  <h3 className="text-sm font-medium">{stage.name}</h3>
                  <WiringBadge status={stage.status} />
                </div>
                <p className="mt-0.5 text-sm text-foreground-muted">{stage.description}</p>
                <p className={`mt-1.5 text-xs ${stage.status === "not_wired" ? "text-foreground-muted" : "text-foreground"}`}>
                  {stage.detail}
                </p>
              </div>
            </li>
          ))}
        </ol>
      </Section>

      <Section
        title="Substrate CRDs"
        subtitle={`Custom resources the pipeline depends on, as installed in ${system.namespace}.`}
      >
        <ul className="grid gap-2 sm:grid-cols-2">
          {system.crds.map((crd) => (
            <li key={crd.name} className="flex items-center justify-between rounded-lg border border-border bg-surface px-3 py-2">
              <code className="font-mono text-xs">{crd.name}</code>
              <WiringBadge status={crd.installed ? "live" : "not_wired"} />
            </li>
          ))}
        </ul>
      </Section>
    </div>
  );
}
