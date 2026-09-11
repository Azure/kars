// kars Bridge — compact metric tile for the command-center + console overviews.
// Shares the premium value-first treatment of `ui.tsx`'s <Stat/> so KPI tiles
// look identical in the Workspace and the Operator Console (one visual
// language). API is unchanged — every existing caller upgrades for free.

const VALUE_TONE = {
  default: "text-foreground",
  ok: "text-ok",
  warning: "text-warning",
  danger: "text-danger",
} as const;

const GLOW_TONE = {
  default: "bg-signal/10",
  ok: "bg-ok/15",
  warning: "bg-warning/15",
  danger: "bg-danger/15",
} as const;

export function StatCard({
  label,
  value,
  tone = "default",
  hint,
}: {
  label: string;
  value: string | number;
  tone?: "default" | "ok" | "warning" | "danger";
  hint?: string;
}) {
  const emphasized = tone !== "default";
  return (
    <div
      className={`group relative overflow-hidden rounded-xl border p-4 shadow-sm transition hover:-translate-y-0.5 hover:shadow-md ${
        emphasized
          ? "border-current/20 bg-gradient-to-br from-surface to-surface-muted/40"
          : "border-border bg-surface"
      }`}
    >
      <span
        aria-hidden
        className={`pointer-events-none absolute -right-6 -top-6 h-16 w-16 rounded-full blur-2xl transition-opacity ${GLOW_TONE[tone]} ${
          emphasized ? "opacity-100" : "opacity-0 group-hover:opacity-100"
        }`}
      />
      <p
        className={`text-[1.7rem] font-semibold leading-none tracking-tight tabular-nums ${VALUE_TONE[tone]}`}
      >
        {value}
      </p>
      <p className="mt-1.5 text-xs font-medium text-foreground-muted">{label}</p>
      {hint && <p className="mt-1 text-[11px] text-foreground-muted/80">{hint}</p>}
    </div>
  );
}
