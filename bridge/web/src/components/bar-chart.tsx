// kars Bridge — simple, dependency-free horizontal bar chart for count data.
// Renders real values only; an empty series renders nothing (caller shows the
// honest empty state).

export function BarChart({
  data,
  colorClass = "bg-signal",
}: {
  data: Array<{ label: string; count: number }>;
  colorClass?: string;
}) {
  const max = Math.max(1, ...data.map((d) => d.count));
  return (
    <ul className="space-y-2.5">
      {data.map((d) => (
        <li key={d.label} className="flex items-center gap-3">
          <span className="w-24 shrink-0 truncate text-xs text-foreground-muted">{d.label}</span>
          <div className="relative h-5 flex-1 overflow-hidden rounded-md bg-surface-muted ring-1 ring-inset ring-border/60">
            <div
              className={`relative h-full rounded-md ${colorClass} transition-[width] duration-500 ease-out`}
              style={{ width: `${Math.max(4, (d.count / max) * 100)}%` }}
            >
              {/* subtle top sheen for a lit, premium fill */}
              <span
                aria-hidden
                className="absolute inset-x-0 top-0 h-1/2 rounded-t-md bg-gradient-to-b from-white/25 to-transparent"
              />
            </div>
          </div>
          <span className="w-8 shrink-0 text-right text-xs font-medium tabular-nums">{d.count}</span>
        </li>
      ))}
    </ul>
  );
}
