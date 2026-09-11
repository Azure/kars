// kars Bridge web — map a KarsTask phase to a status-badge tone.

import { StatusBadge } from "@/components/status-badge";

type Tone = "ok" | "warning" | "danger" | "muted";

function phaseTone(phase: string): Tone {
  switch (phase) {
    case "Ready":
      return "ok";
    case "Degraded":
      return "danger";
    case "Pending":
      return "warning";
    default:
      return "muted";
  }
}

export function PhaseBadge({ phase }: { phase: string }) {
  return <StatusBadge tone={phaseTone(phase)} label={phase} />;
}
