// kars Bridge web — wiring-status badge. Communicates honest implementation
// status: live (green), partial (amber), not wired (neutral/dashed).

import type { WiringStatus } from "@/lib/types";
import { WIRING_LABELS } from "@/lib/types";

const STYLE: Record<WiringStatus, string> = {
  live: "border-ok/30 bg-ok/15 text-ok",
  partial: "border-warning/30 bg-warning/15 text-warning",
  not_wired: "border-border bg-surface-muted text-foreground-muted",
};

export function WiringBadge({ status }: { status: WiringStatus }) {
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs font-medium ${STYLE[status]}`}
    >
      <span
        aria-hidden
        className={
          status === "not_wired"
            ? "h-1.5 w-1.5 rounded-full border border-current"
            : "h-1.5 w-1.5 rounded-full bg-current"
        }
      />
      {WIRING_LABELS[status]}
    </span>
  );
}
