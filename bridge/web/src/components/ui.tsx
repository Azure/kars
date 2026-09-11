// kars Bridge — shared UI primitives. One consistent visual language so every
// page stops being an undifferentiated stack of gray boxes. Hierarchy comes
// from these: PageHeader (eyebrow + title + lead), Section (titled block),
// Card (hoverable surface), Stat (KPI), Badge (status), Skeleton (loading).

import type { ReactNode } from "react";

export function PageHeader({ eyebrow, title, lead, action }: { eyebrow?: string; title: string; lead?: string; action?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-4 border-b border-border/70 pb-4">
      <div className="max-w-2xl">
        {eyebrow && (
          <p className="mb-1.5 inline-flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wider text-signal">
            <span aria-hidden className="inline-block h-1 w-1 rounded-full bg-signal" />
            {eyebrow}
          </p>
        )}
        <h1 className="text-2xl font-semibold tracking-tight sm:text-[1.7rem]">{title}</h1>
        {lead && <p className="mt-1.5 text-sm leading-relaxed text-foreground-muted">{lead}</p>}
      </div>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  );
}

export function Section({ title, subtitle, action, children, className = "" }: { title?: string; subtitle?: string; action?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={`kb-card p-5 sm:p-6 ${className}`}>
      {(title || action) && (
        <div className="mb-4 flex items-start justify-between gap-3">
          <div>
            {title && <h2 className="text-sm font-semibold">{title}</h2>}
            {subtitle && <p className="mt-0.5 text-xs text-foreground-muted">{subtitle}</p>}
          </div>
          {action}
        </div>
      )}
      {children}
    </section>
  );
}

export function Card({ children, className = "", hover = false, accent = false }: { children: ReactNode; className?: string; hover?: boolean; accent?: boolean }) {
  return (
    <div className={`kb-card ${hover ? "kb-card-hover" : ""} ${accent ? "border-signal/30 bg-signal/[0.04]" : ""} p-4 ${className}`}>{children}</div>
  );
}

export function Stat({ label, value, hint, accent }: { label: string; value: ReactNode; hint?: string; accent?: boolean }) {
  return (
    <div className={`group relative overflow-hidden rounded-xl border p-4 shadow-sm transition hover:shadow-md ${accent ? "border-signal/30 bg-gradient-to-br from-signal/[0.08] to-transparent" : "border-border bg-surface"}`}>
      {accent && <span aria-hidden className="pointer-events-none absolute -right-6 -top-6 h-16 w-16 rounded-full bg-signal/10 blur-2xl" />}
      <p className={`text-[1.7rem] font-semibold tabular-nums leading-none tracking-tight ${accent ? "text-signal" : "text-foreground"}`}>{value}</p>
      <p className="mt-1.5 text-xs font-medium text-foreground-muted">{label}</p>
      {hint && <p className="mt-1 text-[11px] text-foreground-muted/80">{hint}</p>}
    </div>
  );
}

type Tone = "ok" | "warn" | "danger" | "info" | "muted" | "accent";
const TONE: Record<Tone, string> = {
  ok: "border-ok/30 bg-ok/10 text-ok",
  warn: "border-warning/30 bg-warning/10 text-warning",
  danger: "border-danger/30 bg-danger/10 text-danger",
  info: "border-signal/30 bg-signal/10 text-signal",
  accent: "border-accent/30 bg-accent/10 text-accent",
  muted: "border-border bg-surface-muted text-foreground-muted",
};
export function Badge({ tone = "muted", children, dot = false }: { tone?: Tone; children: ReactNode; dot?: boolean }) {
  return (
    <span className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-medium ${TONE[tone]}`}>
      {dot && <span className="h-1.5 w-1.5 rounded-full bg-current" />}
      {children}
    </span>
  );
}

export function Skeleton({ className = "" }: { className?: string }) {
  return <div className={`kb-skeleton ${className}`} />;
}
