// kars Bridge Operator Console — Datapath witness.
//
// Surfaces the OPTIONAL eBPF (Inspektor Gadget) datapath-completeness witness:
// an independent, kernel-level attestation of what each sandbox ACTUALLY sends
// on the network, cross-checked against the controller-declared egress
// allowlist. The Bridge only reads the `kars-datapath-witness` ConfigMap the
// witness publishes — enforcement stays with the router proxy + NetworkPolicy;
// this page only ATTESTS. When the witness isn't installed, we show honest
// enable instructions — never fabricated data.

import { PageHeader, Stat, Badge } from "@/components/ui";
import { Icon } from "@/components/icon";
import { getDatapathWitness } from "@/lib/bff";
import type { DatapathWitness, DatapathWitnessSandbox } from "@/lib/types";

export const dynamic = "force-dynamic";

function verdictTone(v: string): "ok" | "warn" | "muted" {
  if (v === "COMPLIANT") return "ok";
  if (v === "BEYOND-DECLARED") return "warn";
  return "muted";
}

function verdictLabel(v: string): string {
  if (v === "COMPLIANT") return "Compliant";
  if (v === "BEYOND-DECLARED") return "Beyond declared";
  if (v === "LEARN") return "Learn / unconstrained";
  return v;
}

export default async function DatapathWitnessPage() {
  let witness: DatapathWitness | null = null;
  let error = false;
  try {
    witness = await getDatapathWitness();
  } catch {
    error = true;
  }

  const enabled = !!witness?.enabled;
  const sandboxes = witness?.sandboxes ?? [];
  const beyond = sandboxes.filter((s) => s.verdict === "BEYOND-DECLARED").length;
  const compliant = sandboxes.filter((s) => s.verdict === "COMPLIANT").length;
  const learn = sandboxes.filter((s) => s.verdict === "LEARN").length;

  return (
    <div className="space-y-6">
      <PageHeader
        eyebrow="Operator Console"
        title="Datapath witness"
        lead="An independent, kernel-level (eBPF) attestation of what each sandbox actually sends on the network — cross-checked against the controller-declared egress allowlist. Enforcement stays with the router proxy and NetworkPolicy; this witness only attests."
      />

      {error && (
        <div className="rounded-xl border border-danger/30 bg-danger/[0.06] px-4 py-3 text-sm text-foreground-muted">
          Couldn&apos;t reach the cluster to read the witness. Check the BFF&apos;s cluster wiring.
        </div>
      )}

      {!error && !enabled && <NotEnabled hint={witness?.install_hint} />}

      {!error && enabled && (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            <Stat label="Sandboxes witnessed" value={sandboxes.length} />
            <Stat label="Beyond declared" value={beyond} accent={beyond > 0} />
            <Stat label="Compliant" value={compliant} />
            <Stat label="Learn / unconstrained" value={learn} />
          </div>

          <div className="flex flex-wrap items-center gap-2 text-xs text-foreground-muted">
            <span aria-hidden className="inline-flex h-1.5 w-1.5 rounded-full bg-ok kb-pulse" />
            Live from the eBPF witness
            {witness?.generated_at && (
              <span>· last observed {new Date(witness.generated_at).toLocaleTimeString()}</span>
            )}
            {witness?.window_seconds && <span>· {witness.window_seconds}s capture window</span>}
          </div>

          {sandboxes.length === 0 ? (
            <div className="rounded-xl border border-dashed border-border bg-surface-muted/30 px-4 py-8 text-center text-sm text-foreground-muted">
              The witness is running but hasn&apos;t observed any sandbox egress yet.
            </div>
          ) : (
            <ul className="space-y-3">
              {sandboxes.map((s) => (
                <WitnessRow key={s.namespace} s={s} />
              ))}
            </ul>
          )}

          <p className="text-xs leading-relaxed text-foreground-muted">
            <span className="font-medium text-foreground">How to read this.</span> DNS = host
            intent; TCP connects = the actual external datapath. A{" "}
            <span className="font-medium text-warning">beyond-declared</span> host means the kernel
            observed egress to a host that isn&apos;t in the sandbox&apos;s signed allowlist — in{" "}
            <code className="rounded bg-surface-muted px-1">strict</code> mode the router proxy
            should have blocked the connect; a DNS-only observation is intent without a connect.{" "}
            <span className="font-medium text-foreground-muted">Learn</span> means no host allowlist
            is published yet — the observed set is the baseline you would promote into strict.
          </p>
        </>
      )}
    </div>
  );
}

function WitnessRow({ s }: { s: DatapathWitnessSandbox }) {
  return (
    <li className="kb-card p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="font-mono text-sm font-medium">{s.sandbox}</p>
          <p className="mt-0.5 font-mono text-[11px] text-foreground-muted">{s.namespace}</p>
        </div>
        <Badge tone={verdictTone(s.verdict)} dot>
          {verdictLabel(s.verdict)}
        </Badge>
      </div>

      <div className="mt-3 grid gap-3 sm:grid-cols-3">
        <HostSet label="Declared allowlist" hosts={s.declared_hosts} empty="none (unconstrained)" />
        <HostSet
          label="Observed (DNS)"
          hosts={s.observed_dns}
          empty="none in window"
          highlight={new Set(s.beyond_declared)}
        />
        <div className="min-w-0">
          <p className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
            External connects
          </p>
          <p className="text-sm tabular-nums">{s.observed_connects}</p>
          {s.beyond_declared.length > 0 && (
            <p className="mt-1 text-[11px] text-warning">
              {s.beyond_declared.length} beyond declared
            </p>
          )}
        </div>
      </div>
    </li>
  );
}

/** The K8s stub resolver expands a ClusterIP name with the node's DNS search
 *  domain, so the eBPF witness observes both the real name and search-expanded
 *  variants like `foo.ns.svc.cluster.local.<node-suffix>.internal.cloudapp.net`.
 *  Trim back to the meaningful in-cluster name so the list is readable (the full
 *  observed string stays available on hover). */
function cleanHost(h: string): string {
  const marker = ".svc.cluster.local";
  const idx = h.indexOf(marker);
  if (idx !== -1) return h.slice(0, idx + marker.length);
  return h;
}

function HostSet({
  label,
  hosts,
  empty,
  highlight,
}: {
  label: string;
  hosts: string[];
  empty: string;
  highlight?: Set<string>;
}) {
  // Collapse search-domain-expanded duplicates to one readable entry, carrying
  // the beyond-declared flag if ANY raw variant was flagged, and keeping the
  // longest raw form for the hover title.
  const grouped = new Map<string, { flagged: boolean; raw: string }>();
  for (const h of hosts) {
    const c = cleanHost(h);
    const prev = grouped.get(c);
    grouped.set(c, {
      flagged: (prev?.flagged ?? false) || (highlight?.has(h) ?? false),
      raw: prev && prev.raw.length >= h.length ? prev.raw : h,
    });
  }
  const items = Array.from(grouped.entries());

  return (
    <div className="min-w-0">
      <p className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
        {label}
      </p>
      {items.length === 0 ? (
        <p className="text-xs italic text-foreground-muted/70">{empty}</p>
      ) : (
        <ul className="flex flex-col gap-1">
          {items.map(([display, { flagged, raw }]) => (
            <li
              key={display}
              title={raw}
              className={`block break-all rounded border px-1.5 py-0.5 font-mono text-[11px] leading-snug ${
                flagged
                  ? "border-warning/40 bg-warning/10 text-warning"
                  : "border-border bg-surface-muted text-foreground-muted"
              }`}
            >
              {display}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function NotEnabled({ hint }: { hint?: string }) {
  return (
    <div className="kb-card p-6">
      <div className="flex items-start gap-3">
        <span aria-hidden className="text-xl">
          <Icon name="eye" size={22} />
        </span>
        <div className="min-w-0 space-y-3">
          <div>
            <h2 className="text-sm font-semibold">Datapath witness not enabled</h2>
            <p className="mt-1 text-sm text-foreground-muted">
              The eBPF witness is optional and off by default — it installs a privileged Inspektor
              Gadget DaemonSet plus a small aggregator. When enabled, every sandbox&apos;s
              kernel-observed egress is cross-checked here against its declared allowlist, live.
            </p>
          </div>
          <div>
            <p className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-foreground-muted">
              Enable on the cluster
            </p>
            <pre className="overflow-x-auto rounded-lg border border-border bg-surface-muted p-3 font-mono text-xs">
              {hint ?? "KARS_EBPF_WITNESS=1 deploy/ebpf-witness/install.sh --continuous"}
            </pre>
          </div>
          <p className="text-xs text-foreground-muted">
            Requires a Linux kernel with BTF on every node. Read-only: the witness never blocks or
            modifies traffic. See{" "}
            <code className="rounded bg-surface-muted px-1">deploy/ebpf-witness/README.md</code>.
          </p>
        </div>
      </div>
    </div>
  );
}
