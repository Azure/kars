// kars Bridge web — status badge. A small, accessible indicator used across
// the trust/verification surfaces.

type Tone = "ok" | "warning" | "danger" | "muted";

const TONE_CLASS: Record<Tone, string> = {
  ok: "bg-ok/15 text-ok border-ok/30",
  warning: "bg-warning/15 text-warning border-warning/30",
  danger: "bg-danger/15 text-danger border-danger/30",
  muted: "bg-surface-muted text-foreground-muted border-border",
};

export function StatusBadge({
  tone,
  label,
}: {
  tone: Tone;
  label: string;
}) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-medium ${TONE_CLASS[tone]}`}
    >
      <span
        aria-hidden
        className="h-1.5 w-1.5 rounded-full bg-current"
      />
      {label}
    </span>
  );
}
