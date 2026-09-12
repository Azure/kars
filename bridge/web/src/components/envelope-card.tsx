// kars Bridge web — trust-envelope visualization.
//
// Renders the authority a task holds as a precise, scannable card: the
// autonomy tier (as the same 1–5 scale used in the create form), the
// authority ceiling that bounds descendants, the delegation budget, and the
// policy bounds. Permissive defaults (no tool policy / no egress allow-list)
// are surfaced as explicit, mildly-cautioned statements — to a CISO an empty
// allow-list is a finding, not a blank.

import { StatusBadge } from "@/components/status-badge";
import { formatInt, formatUsdMicros } from "@/lib/format";
import { TIER_LABELS, type Envelope } from "@/lib/types";

function TierScale({ value, ceiling }: { value: number; ceiling: number }) {
  return (
    <div className="flex items-center gap-1.5" role="img"
      aria-label={`Tier ${value} of 5; authority ceiling at tier ${ceiling}`}>
      {[1, 2, 3, 4, 5].map((t) => {
        const active = t <= value;
        const isCeiling = t === ceiling;
        return (
          <span
            key={t}
            title={`Tier ${t} — ${TIER_LABELS[t]}`}
            className={[
              "h-6 w-6 rounded grid place-items-center text-xs font-semibold border",
              active
                ? "bg-signal/15 text-signal border-signal/40"
                : "bg-surface-muted text-foreground-muted border-border",
              isCeiling
                ? "ring-2 ring-warning ring-offset-1 ring-offset-surface"
                : "",
            ].join(" ")}
          >
            {t}
          </span>
        );
      })}
    </div>
  );
}

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 py-2">
      <dt className="text-sm text-foreground-muted">{label}</dt>
      <dd className="text-sm font-medium tabular-nums">{children}</dd>
    </div>
  );
}

/** A permissive-default value: shown as an explicit, mildly-cautioned fact. */
function PolicyBound({ value }: { value: string | null }) {
  if (value) {
    return <span className="font-mono text-xs">{value}</span>;
  }
  return (
    <span className="inline-flex items-center gap-1 text-xs font-medium text-warning">
      <svg aria-hidden viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="currentColor">
        <path d="M8 1.5 1 14h14L8 1.5Zm0 4.25a.75.75 0 0 1 .75.75v3a.75.75 0 0 1-1.5 0v-3A.75.75 0 0 1 8 5.75ZM8 11a.9.9 0 1 0 0 1.8A.9.9 0 0 0 8 11Z" />
      </svg>
      none — unrestricted
    </span>
  );
}

export function EnvelopeCard({ envelope }: { envelope: Envelope }) {
  const usd = formatUsdMicros(envelope.budget?.usd_micros ?? null);
  const tokens = envelope.budget?.tokens ?? null;

  return (
    <section
      aria-labelledby="envelope-heading"
      className="rounded-xl border border-border bg-surface p-6"
    >
      <div className="flex items-center justify-between">
        <h2 id="envelope-heading" className="text-sm font-semibold">
          Trust envelope
        </h2>
        <StatusBadge
          tone="muted"
          label={`Tier ${envelope.tier} · ${TIER_LABELS[envelope.tier] ?? "?"}`}
        />
      </div>

      <div className="mt-4">
        <div className="flex items-center justify-between">
          <span className="text-sm text-foreground-muted">Autonomy</span>
          <TierScale value={envelope.tier} ceiling={envelope.authority_ceiling} />
        </div>
        <p className="mt-2 text-xs text-foreground-muted">
          Filled cells show the tier this task holds. The{" "}
          <span className="text-warning">ringed</span> cell is the authority
          ceiling — the highest tier any delegated child may hold.
        </p>
      </div>

      <dl className="mt-4 divide-y divide-border">
        <Field label="Authority ceiling">
          Tier {envelope.authority_ceiling} ·{" "}
          {TIER_LABELS[envelope.authority_ceiling] ?? "?"}
        </Field>
        <Field label="Delegation depth remaining">
          {envelope.delegation_depth}
        </Field>
        <Field label="Token budget">
          {tokens != null ? (
            <>
              {formatInt(tokens)}{" "}
              <span className="text-xs font-normal text-foreground-muted">
                tokens / subtree
              </span>
            </>
          ) : (
            "—"
          )}
        </Field>
        <Field label="Spend budget">
          {usd ? (
            <>
              {usd}{" "}
              <span className="text-xs font-normal text-foreground-muted">
                / subtree
              </span>
            </>
          ) : (
            "—"
          )}
        </Field>
        <Field label="Tool policy">
          <PolicyBound value={envelope.tool_policy} />
        </Field>
        <Field label="Egress allow-list">
          <PolicyBound value={envelope.egress_allowlist} />
        </Field>
      </dl>
    </section>
  );
}
