"use client";

// kars Bridge — Network mode (learning → enforced). Surfaces the sandbox's REAL
// egress enforcement mode (KarsSandbox.networkPolicy.egressMode) and lets the
// operator promote it: start in Learning (the agent reaches anything, every
// domain recorded by the router), review what it actually reached, then flip to
// Enforced (only the approved allowlist passes). The flip drives the real lever
// — it pins the mission's egress allowlist, which the controller compiles into
// Strict mode on the next reconcile. Nothing here is cosmetic.

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { Icon } from "@/components/icon";

type LearnedResp = {
  available: boolean;
  mode?: string;
  domains?: string[];
  enforced?: string[];
  reason?: string;
};

export function NetworkMode({
  ns,
  task,
  mode,
}: {
  ns: string;
  task: string;
  mode: string | null;
}) {
  const enforced = mode === "Strict";
  const [data, setData] = useState<LearnedResp | null>(null);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [manual, setManual] = useState("");
  const [busy, setBusy] = useState(false);
  const router = useRouter();

  useEffect(() => {
    let on = true;
    fetch(`/api/namespaces/${ns}/tasks/${task}/egress/learned`)
      .then((r) => r.json())
      .then((d: LearnedResp) => {
        if (!on) return;
        setData(d);
        // Pre-select all learned domains for convenience.
        const pre: Record<string, boolean> = {};
        (d.domains ?? []).forEach((h) => (pre[h] = true));
        setSelected(pre);
      })
      .catch(() => on && setData({ available: false }))
      .finally(() => on && setLoading(false));
    return () => {
      on = false;
    };
  }, [ns, task]);

  async function flip(toEnforced: boolean) {
    setBusy(true);
    try {
      const allow = toEnforced
        ? [
            ...Object.entries(selected).filter(([, v]) => v).map(([h]) => h),
            ...manual.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean),
          ]
        : [];
      await fetch(`/api/namespaces/${ns}/tasks/${task}/egress-mode`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ mode: toEnforced ? "enforced" : "learning", allow }),
      });
      router.refresh();
    } finally {
      setBusy(false);
    }
  }

  const learned = data?.domains ?? [];
  const enforcedList = data?.enforced ?? [];
  const toggle = (h: string) => setSelected((s) => ({ ...s, [h]: !s[h] }));
  const selectedCount = Object.values(selected).filter(Boolean).length + manual.split(/[\s,]+/).filter((s) => s.trim()).length;

  return (
    <section className="rounded-xl border border-border bg-surface p-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">Network enforcement</h2>
          <p className="mt-0.5 max-w-xl text-xs text-foreground-muted">
            {enforced
              ? "Enforced — the sandbox denies any destination outside the approved allowlist at the boundary."
              : "Learning — the agent can reach the network while the router records every domain it touches. Review what it actually reaches, then enforce when you're confident."}
          </p>
        </div>
        <span
          className={`shrink-0 rounded-full border px-2.5 py-1 text-xs font-medium ${
            enforced
              ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
              : "border-sky-500/30 bg-sky-500/10 text-sky-600"
          }`}
        >
          {enforced ? <span className="inline-flex items-center gap-1"><Icon name="lock" size={12} /> Enforced</span> : <span className="inline-flex items-center gap-1"><Icon name="eye" size={12} /> Learning</span>}
        </span>
      </div>

      {enforced ? (
        // ── Enforced: show the allowlist, offer back-to-learning. ──────────
        <div className="mt-4">
          <p className="text-[11px] uppercase tracking-wide text-foreground-muted">Allowed destinations</p>
          {enforcedList.length === 0 ? (
            <p className="mt-1 text-sm text-foreground-muted">Model path only — all other egress denied.</p>
          ) : (
            <ul className="mt-2 flex flex-wrap gap-1.5">
              {enforcedList.map((h) => (
                <li key={h} className="inline-flex items-center gap-1.5 rounded-full bg-surface-muted px-2.5 py-1 font-mono text-xs">
                  <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" aria-hidden />
                  {h}
                </li>
              ))}
            </ul>
          )}
          <button
            type="button"
            onClick={() => flip(false)}
            disabled={busy}
            className="mt-4 rounded-lg border border-border px-3 py-1.5 text-xs font-medium hover:bg-surface-muted disabled:opacity-50"
          >
            {busy ? "Applying…" : "← Back to learning"}
          </button>
        </div>
      ) : (
        // ── Learning: show learned domains, build allowlist, enforce. ──────
        <div className="mt-4">
          <p className="text-[11px] uppercase tracking-wide text-foreground-muted">Domains this agent has reached</p>
          {loading ? (
            <p className="mt-1 text-sm text-foreground-muted">Reading the router&rsquo;s observation buffer…</p>
          ) : learned.length > 0 ? (
            <ul className="mt-2 space-y-1.5">
              {learned.map((h) => (
                <li key={h} className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={!!selected[h]}
                    onChange={() => toggle(h)}
                    className="h-3.5 w-3.5 rounded border-border"
                  />
                  <span className="font-mono text-xs">{h}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="mt-1 text-sm text-foreground-muted">
              {data?.available === false
                ? "No observed domains surfaced yet — the agent hasn't reached out, or the router's observation buffer isn't readable from here. You can still enforce an allowlist below."
                : "Nothing reached the network yet."}
            </p>
          )}

          <div className="mt-4">
            <label className="text-[11px] font-medium text-foreground-muted">Add destinations (host or host:port, comma/space separated)</label>
            <input
              value={manual}
              onChange={(e) => setManual(e.target.value)}
              placeholder="e.g. api.github.com:443, registry.npmjs.org"
              className="mt-1 w-full rounded-lg border border-border bg-surface px-3 py-2 font-mono text-xs"
            />
          </div>

          <div className="mt-4 flex items-center gap-3">
            <button
              type="button"
              onClick={() => flip(true)}
              disabled={busy || selectedCount === 0}
              className="rounded-lg bg-signal px-4 py-2 text-xs font-semibold text-signal-fg hover:opacity-90 disabled:opacity-50"
              title={selectedCount === 0 ? "Select or add at least one destination to enforce" : undefined}
            >
              {busy ? "Enforcing…" : <span className="inline-flex items-center gap-1"><Icon name="lock" size={12} /> Enforce {selectedCount} destination{selectedCount === 1 ? "" : "s"}</span>}
            </button>
            <p className="text-[11px] text-foreground-muted">
              Enforcing pins this allowlist; the sandbox then denies everything else at the boundary.
            </p>
          </div>
        </div>
      )}
    </section>
  );
}
