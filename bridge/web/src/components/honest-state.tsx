// kars Bridge — the honesty grammar.
//
// A single, reusable component for the three distinct "no data" situations the
// product must never conflate (UX spec §4):
//   - empty      : a legitimate zero — encourage the next action.
//   - not_wired  : a capability is genuinely absent — state it factually.
//   - needs_run  : the feature exists but has no data until a real run happens.
//
// Conflating "no data source" with "value is 0" is the most common honesty
// failure; this component keeps them visibly different. Never error-red.

type Variant = "empty" | "not_wired" | "needs_run";

const ICONS: Record<Variant, React.ReactNode> = {
  empty: (
    <svg viewBox="0 0 24 24" fill="none" className="h-6 w-6" aria-hidden>
      <path
        d="M12 5v14M5 12h14"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
      />
    </svg>
  ),
  not_wired: (
    <svg viewBox="0 0 24 24" fill="none" className="h-6 w-6" aria-hidden>
      <path
        d="M9 17H7A5 5 0 0 1 7 7h1m6 10h2a5 5 0 0 0 0-10h-1M8 12h8"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
        strokeDasharray="2 2.5"
      />
    </svg>
  ),
  needs_run: (
    <svg viewBox="0 0 24 24" fill="none" className="h-6 w-6" aria-hidden>
      <circle cx="12" cy="12" r="8" stroke="currentColor" strokeWidth="1.6" />
      <path
        d="M12 8v4l2.5 2"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinecap="round"
      />
    </svg>
  ),
};

export function HonestState({
  variant,
  title,
  detail,
  action,
  compact = false,
}: {
  variant: Variant;
  title: string;
  detail?: string;
  action?: React.ReactNode;
  compact?: boolean;
}) {
  return (
    <div
      className={[
        "flex flex-col items-center justify-center rounded-xl border border-dashed border-border bg-surface-muted/40 text-center",
        compact ? "px-6 py-8" : "px-6 py-14",
      ].join(" ")}
    >
      <span className="grid h-11 w-11 place-items-center rounded-full bg-surface text-foreground-muted">
        {ICONS[variant]}
      </span>
      <p className="mt-3 text-sm font-medium text-foreground">{title}</p>
      {detail && (
        <p className="mt-1 max-w-md text-xs text-foreground-muted">{detail}</p>
      )}
      {action && <div className="mt-4">{action}</div>}
    </div>
  );
}
