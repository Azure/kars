"use client";

// kars Bridge Operator Console — hierarchical, EDITABLE inference token budgets.
//
// The aggregate levels above the per-sandbox policy: a cluster-wide cap and
// per-workspace caps, each with a live measured daily-usage meter and one of
// three enforcement modes:
//   • passive — alert-only; never blocks a launch.
//   • buffer  — allows up to +N% headroom, then blocks (admin must raise).
//   • strict  — blocks at 100%; only an admin can raise.
// Edits PUT straight to /api/operator/inference-budgets/* and re-read the live
// hierarchy (usage recomputed server-side from completed runs).

import { useCallback, useEffect, useState } from "react";
import type { InferenceBudgets, BudgetLevel } from "@/lib/types";
import { Icon } from "@/components/icon";

const MODES = [
  { id: "passive", label: "Passive — alert only" },
  { id: "buffer", label: "Buffer — allow +headroom, then block" },
  { id: "strict", label: "Strict — block at limit" },
] as const;

const STATUS_META: Record<
  BudgetLevel["status"],
  { label: string; tone: string; bar: string }
> = {
  ok: { label: "Within budget", tone: "text-signal", bar: "bg-signal" },
  alert: { label: "Over budget — alerting", tone: "text-warning", bar: "bg-warning" },
  over_buffer_headroom: { label: "In buffer headroom", tone: "text-warning", bar: "bg-warning" },
  blocking: { label: "Blocking new work", tone: "text-danger", bar: "bg-danger" },
};

function fmt(n: number): string {
  return n.toLocaleString();
}

export function InferenceBudgets({ isAdmin = true }: { isAdmin?: boolean }) {
  const [data, setData] = useState<InferenceBudgets | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [addingNs, setAddingNs] = useState(false);
  const [addingUser, setAddingUser] = useState(false);

  const load = useCallback(async () => {
    try {
      const r = await fetch("/api/operator/inference-budgets", { cache: "no-store" });
      if (!r.ok) throw new Error();
      setData(await r.json());
      setError(null);
    } catch {
      setError("Couldn't load inference budgets.");
    }
  }, []);

  useEffect(() => {
    const timer = window.setTimeout(() => void load(), 0);
    return () => window.clearTimeout(timer);
  }, [load]);

  if (error) {
    return <p className="text-xs text-danger">{error}</p>;
  }
  if (!data) {
    return <p className="text-xs text-foreground-muted">Loading measured usage…</p>;
  }

  const clusterUsed = data.cluster_used_today;

  return (
    <div className="space-y-4">
      <p className="text-xs text-foreground-muted">
        Aggregate caps over inference token spend, above the per-sandbox policy. The meter is the
        real daily utilization measured from completed runs (UTC day). Passive alerts; buffer allows
        a headroom then blocks; strict blocks at the limit — only an admin raises it.
      </p>

      {/* Active budget alerts — the real "raise alerts" surface (item 2 passive
          mode + any breach). Prominent so a breach isn't buried in a meter. */}
      {(data.alerts?.length ?? 0) > 0 && (
        <div className="space-y-1.5">
          {data.alerts!.map((a) => (
            <div
              key={`${a.scope}-${a.severity}`}
              className={`flex items-start gap-2 rounded-lg border px-3 py-2 text-xs ${
                a.severity === "blocking"
                  ? "border-danger/40 bg-danger/[0.06] text-danger"
                  : "border-warning/40 bg-warning/[0.06] text-warning"
              }`}
            >
              <span aria-hidden>{a.severity === "blocking" ? <Icon name="cross" size={13} /> : <Icon name="warning" size={13} />}</span>
              <span>{a.message}</span>
            </div>
          ))}
        </div>
      )}

      {/* Cluster level */}
      <BudgetCard
        title="Cluster"
        subtitle="The whole cluster's daily inference token cap."
        level={data.cluster}
        fallbackUsed={clusterUsed}
        canEdit={isAdmin}
        onSave={async (body) => {
          await fetch("/api/operator/inference-budgets/cluster", {
            method: "PUT",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          });
          await load();
        }}
      />

      {/* Workspace levels */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <h4 className="text-xs font-semibold text-foreground-muted">Workspaces</h4>
          {!addingNs && isAdmin && (
            <button
              type="button"
              onClick={() => setAddingNs(true)}
              className="rounded-md border border-border px-2 py-1 text-[11px] font-medium hover:bg-surface-muted"
            >
              + Add workspace budget
            </button>
          )}
        </div>
        {data.workspaces.length === 0 && !addingNs && (
          <p className="text-[11px] text-foreground-muted">
            No per-workspace caps. Add one to bound a specific workspace (namespace) below the
            cluster cap.
          </p>
        )}
        {data.workspaces.map((w) => (
          <BudgetCard
            key={w.scope}
            title={w.label}
            subtitle={`Workspace “${w.scope}” daily cap.`}
            level={w}
            fallbackUsed={w.used_today}
            canEdit={isAdmin}
            onSave={async (body) => {
              await fetch(`/api/operator/inference-budgets/workspaces/${encodeURIComponent(w.scope)}`, {
                method: "PUT",
                headers: { "content-type": "application/json" },
                body: JSON.stringify(body),
              });
              await load();
            }}
            onRemove={async () => {
              await fetch(`/api/operator/inference-budgets/workspaces/${encodeURIComponent(w.scope)}`, {
                method: "PUT",
                headers: { "content-type": "application/json" },
                body: JSON.stringify({ clear: true }),
              });
              await load();
            }}
          />
        ))}
        {addingNs && (
          <AddWorkspace
            suggestions={data.unbudgeted_namespaces}
            defaultNs={data.default_namespace}
            onCancel={() => setAddingNs(false)}
            onCreate={async (ns, body) => {
              await fetch(`/api/operator/inference-budgets/workspaces/${encodeURIComponent(ns)}`, {
                method: "PUT",
                headers: { "content-type": "application/json" },
                body: JSON.stringify(body),
              });
              setAddingNs(false);
              await load();
            }}
          />
        )}
      </div>

      {/* Per-user levels (the spec's per-user tier, keyed by created-by). */}
      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <h4 className="text-xs font-semibold text-foreground-muted">Users</h4>
          {!addingUser && isAdmin && (
            <button
              type="button"
              onClick={() => setAddingUser(true)}
              className="rounded-md border border-border px-2 py-1 text-[11px] font-medium hover:bg-surface-muted"
            >
              + Add user budget
            </button>
          )}
        </div>
        {data.users.length === 0 && !addingUser && (
          <p className="text-[11px] text-foreground-muted">
            No per-user caps. Bound an individual user&rsquo;s daily spend below the workspace cap —
            attributed from the mission/team creator.
          </p>
        )}
        {data.users.map((u) => (
          <BudgetCard
            key={u.scope}
            title={u.label}
            subtitle={`User “${u.scope}” daily cap.`}
            level={u}
            fallbackUsed={u.used_today}
            canEdit={isAdmin}
            onSave={async (body) => {
              await fetch(`/api/operator/inference-budgets/users/${encodeURIComponent(u.scope)}`, {
                method: "PUT",
                headers: { "content-type": "application/json" },
                body: JSON.stringify(body),
              });
              await load();
            }}
            onRemove={async () => {
              await fetch(`/api/operator/inference-budgets/users/${encodeURIComponent(u.scope)}`, {
                method: "PUT",
                headers: { "content-type": "application/json" },
                body: JSON.stringify({ clear: true }),
              });
              await load();
            }}
          />
        ))}
        {addingUser && (
          <AddWorkspace
            label="User identity"
            placeholder="e.g. alice@local"
            suggestions={data.unbudgeted_users}
            defaultNs={data.unbudgeted_users[0] ?? ""}
            onCancel={() => setAddingUser(false)}
            onCreate={async (u, body) => {
              await fetch(`/api/operator/inference-budgets/users/${encodeURIComponent(u)}`, {
                method: "PUT",
                headers: { "content-type": "application/json" },
                body: JSON.stringify(body),
              });
              setAddingUser(false);
              await load();
            }}
          />
        )}
      </div>
    </div>
  );
}

function Meter({ level, used }: { level: BudgetLevel | null; used: number }) {
  if (!level || level.daily_tokens === 0) {
    return (
      <p className="text-[11px] text-foreground-muted">
        Measured today: <span className="font-medium text-foreground">{fmt(used)}</span> tokens · no
        cap
      </p>
    );
  }
  const meta = STATUS_META[level.status];
  const pct = Math.min(100, Math.round(level.percent * 100));
  return (
    <div>
      <div className="flex items-center justify-between text-[11px]">
        <span className={meta.tone}>{meta.label}</span>
        <span className="tabular-nums text-foreground-muted">
          {fmt(used)} / {fmt(level.daily_tokens)}
          {level.mode === "buffer" && level.hard_cap > level.daily_tokens
            ? ` (hard ${fmt(level.hard_cap)})`
            : ""}{" "}
          · {Math.round(level.percent * 100)}%
        </span>
      </div>
      <div className="mt-1 h-1.5 w-full overflow-hidden rounded-full bg-surface-muted">
        <div className={`h-full ${meta.bar}`} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

type SaveBody = { daily_tokens: number; mode: string; buffer_percent: number };

function BudgetCard({
  title,
  subtitle,
  level,
  fallbackUsed,
  canEdit = true,
  onSave,
  onRemove,
}: {
  title: string;
  subtitle: string;
  level: BudgetLevel | null;
  fallbackUsed: number;
  canEdit?: boolean;
  onSave: (body: SaveBody) => Promise<void>;
  onRemove?: () => Promise<void>;
}) {
  const [editing, setEditing] = useState(false);
  const [daily, setDaily] = useState(level?.daily_tokens ?? 0);
  const [mode, setMode] = useState<string>(level?.mode ?? "passive");
  const [buffer, setBuffer] = useState(level?.buffer_percent ?? 20);
  const [busy, setBusy] = useState(false);

  return (
    <div className="rounded-lg border border-border bg-surface-muted/30 p-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <p className="text-sm font-medium">{title}</p>
          <p className="text-[11px] text-foreground-muted">{subtitle}</p>
        </div>
        {!editing && (
          <div className="flex items-center gap-2">
            {level && (
              <span className="rounded-full border border-border px-2 py-0.5 text-[10px] font-medium capitalize text-foreground-muted">
                {level.mode}
              </span>
            )}
            {canEdit ? (
              <button
                type="button"
                onClick={() => {
                  setDaily(level?.daily_tokens ?? 0);
                  setMode(level?.mode ?? "passive");
                  setBuffer(level?.buffer_percent ?? 20);
                  setEditing(true);
                }}
                className="rounded-md border border-border px-2 py-1 text-[11px] font-medium hover:bg-surface-muted"
              >
                {level ? "Edit" : "Set budget"}
              </button>
            ) : (
              <span
                className="rounded-md border border-border px-2 py-1 text-[10px] font-medium text-foreground-muted"
                title="Only a cluster or org admin can raise inference budgets."
              >
                <Icon name="lock" size={11} className="inline" /> Admin only
              </span>
            )}
          </div>
        )}
      </div>

      <div className="mt-2">
        <Meter level={level} used={fallbackUsed} />
      </div>

      {editing && (
        <div className="mt-3 space-y-2 rounded-md border border-border bg-surface p-3">
          <label className="block text-[11px] font-medium text-foreground-muted">
            Daily token cap (0 = no cap)
            <input
              type="number"
              min={0}
              value={daily}
              onChange={(e) => setDaily(Number(e.target.value))}
              className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1 text-sm tabular-nums"
            />
          </label>
          <label className="block text-[11px] font-medium text-foreground-muted">
            Enforcement
            <select
              value={mode}
              onChange={(e) => setMode(e.target.value)}
              className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1 text-sm"
            >
              {MODES.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.label}
                </option>
              ))}
            </select>
          </label>
          {mode === "buffer" && (
            <label className="block text-[11px] font-medium text-foreground-muted">
              Buffer headroom (%)
              <input
                type="number"
                min={0}
                max={1000}
                value={buffer}
                onChange={(e) => setBuffer(Number(e.target.value))}
                className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1 text-sm tabular-nums"
              />
            </label>
          )}
          <div className="flex items-center gap-2 pt-1">
            <button
              type="button"
              disabled={busy}
              onClick={async () => {
                setBusy(true);
                await onSave({ daily_tokens: daily, mode, buffer_percent: buffer });
                setBusy(false);
                setEditing(false);
              }}
              className="rounded-md bg-signal px-3 py-1.5 text-[11px] font-semibold text-signal-fg disabled:opacity-50"
            >
              {busy ? "Saving…" : "Save"}
            </button>
            <button
              type="button"
              disabled={busy}
              onClick={() => setEditing(false)}
              className="rounded-md border border-border px-3 py-1.5 text-[11px] text-foreground-muted hover:bg-surface-muted"
            >
              Cancel
            </button>
            {onRemove && level && (
              <button
                type="button"
                disabled={busy}
                onClick={async () => {
                  setBusy(true);
                  await onRemove();
                  setBusy(false);
                  setEditing(false);
                }}
                className="ml-auto rounded-md border border-border px-3 py-1.5 text-[11px] text-foreground-muted hover:border-danger/40 hover:text-danger"
              >
                Remove cap
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function AddWorkspace({
  suggestions,
  defaultNs,
  label = "Workspace (namespace)",
  placeholder,
  onCancel,
  onCreate,
}: {
  suggestions: string[];
  defaultNs: string;
  label?: string;
  placeholder?: string;
  onCancel: () => void;
  onCreate: (ns: string, body: SaveBody) => Promise<void>;
}) {
  const [ns, setNs] = useState(suggestions[0] ?? defaultNs);
  const [daily, setDaily] = useState(50000);
  const [mode, setMode] = useState("buffer");
  const [buffer, setBuffer] = useState(20);
  const [busy, setBusy] = useState(false);

  return (
    <div className="space-y-2 rounded-lg border border-signal/30 bg-signal/[0.03] p-3">
      <label className="block text-[11px] font-medium text-foreground-muted">
        {label}
        <input
          value={ns}
          onChange={(e) => setNs(e.target.value)}
          list="ws-suggestions"
          placeholder={placeholder}
          className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1 text-sm font-mono"
        />
        <datalist id="ws-suggestions">
          {suggestions.map((s) => (
            <option key={s} value={s} />
          ))}
        </datalist>
      </label>
      <label className="block text-[11px] font-medium text-foreground-muted">
        Daily token cap
        <input
          type="number"
          min={0}
          value={daily}
          onChange={(e) => setDaily(Number(e.target.value))}
          className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1 text-sm tabular-nums"
        />
      </label>
      <label className="block text-[11px] font-medium text-foreground-muted">
        Enforcement
        <select
          value={mode}
          onChange={(e) => setMode(e.target.value)}
          className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1 text-sm"
        >
          {MODES.map((m) => (
            <option key={m.id} value={m.id}>
              {m.label}
            </option>
          ))}
        </select>
      </label>
      {mode === "buffer" && (
        <label className="block text-[11px] font-medium text-foreground-muted">
          Buffer headroom (%)
          <input
            type="number"
            min={0}
            max={1000}
            value={buffer}
            onChange={(e) => setBuffer(Number(e.target.value))}
            className="mt-1 w-full rounded-md border border-border bg-surface px-2 py-1 text-sm tabular-nums"
          />
        </label>
      )}
      <div className="flex items-center gap-2 pt-1">
        <button
          type="button"
          disabled={busy || !ns.trim()}
          onClick={async () => {
            setBusy(true);
            await onCreate(ns.trim(), { daily_tokens: daily, mode, buffer_percent: buffer });
            setBusy(false);
          }}
          className="rounded-md bg-signal px-3 py-1.5 text-[11px] font-semibold text-signal-fg disabled:opacity-50"
        >
          {busy ? "Adding…" : "Add"}
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={onCancel}
          className="rounded-md border border-border px-3 py-1.5 text-[11px] text-foreground-muted hover:bg-surface-muted"
        >
          Cancel
        </button>
      </div>
    </div>
  );
}

/** Inline per-sandbox policy budget editor — PATCHes spec.tokenBudget.dailyTokens
 *  so an operator can adjust a policy's daily cap without editing raw JSON. */
export function InferenceBudgetEdit({ name, current }: { name: string; current: number | null }) {
  const [editing, setEditing] = useState(false);
  const [val, setVal] = useState(current ?? 0);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);

  if (!editing) {
    return (
      <button
        type="button"
        onClick={() => {
          setVal(current ?? 0);
          setEditing(true);
          setSaved(false);
        }}
        className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 tabular-nums hover:bg-surface-muted"
        title="Edit daily token budget"
      >
        {saved ? <span className="text-signal">{(current ?? 0).toLocaleString()} ✓</span> : (current != null ? current.toLocaleString() : "—")}
        <span aria-hidden className="text-foreground-muted"><Icon name="pencil" size={10} /></span>
      </button>
    );
  }
  return (
    <span className="inline-flex items-center gap-1">
      <input
        type="number"
        min={0}
        value={val}
        onChange={(e) => setVal(Number(e.target.value))}
        className="w-24 rounded border border-border bg-surface px-1.5 py-0.5 text-xs tabular-nums"
      />
      <button
        type="button"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          await fetch(`/api/operator/inferencepolicies/${encodeURIComponent(name)}`, {
            method: "PATCH",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ daily_tokens: val }),
          });
          setBusy(false);
          setEditing(false);
          setSaved(true);
        }}
        className="rounded bg-signal px-1.5 py-0.5 text-[10px] font-semibold text-signal-fg disabled:opacity-50"
      >
        {busy ? "…" : "Save"}
      </button>
      <button
        type="button"
        onClick={() => setEditing(false)}
        className="rounded border border-border px-1.5 py-0.5 text-[10px] text-foreground-muted"
      >
        ✕
      </button>
    </span>
  );
}
