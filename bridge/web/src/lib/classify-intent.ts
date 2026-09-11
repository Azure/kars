// Intent classifier for the unified intent-first intake.
//
// One intent box decides whether the work is a MISSION (a focused, one-off task
// that composes → runs → delivers → done) or a standing TEAM (a continuous org
// that works under a charter, often with cadence and multiple roles). This is a
// transparent client-side heuristic that produces a *recommendation* the user
// can always override — it never silently decides for them. The authoritative
// composition still happens server-side via the orchestrator once routed.

export type IntentKind = "mission" | "team";

export interface IntentClassification {
  kind: IntentKind;
  confidence: "low" | "medium" | "high";
  reason: string;
}

// Signals that the work is ongoing/standing rather than a single deliverable.
const TEAM_PATTERNS: RegExp[] = [
  /\bteams?\b/,
  /\bmonitor(ing|s)?\b/,
  /\bwatch(ing|es)?\b/,
  /\bcontinuous(ly)?\b/,
  /\bongoing\b/,
  /\bstanding\b/,
  /\bkeep an eye\b/,
  /\bover time\b/,
  /\bevery (day|week|hour|morning|month)\b/,
  /\b(daily|weekly|hourly|nightly|monthly)\b/,
  /\bcadence\b/,
  /\bon a schedule\b/,
  /\bregularly\b/,
  /\bkeep .* (healthy|up to date|updated|current)\b/,
  /\btrack .* (over time|continuously)\b/,
  /\b(recurring|recurrent)\b/,
  /\bmultiple (roles|agents|members)\b/,
  /\borg chart\b/,
  /\b(finance|marketing|support|ops|sales) team\b/,
];

// Signals that the work is a discrete, bounded deliverable.
const MISSION_PATTERNS: RegExp[] = [
  /\b(write|draft|create|build|make|generate|produce)\b/,
  /\b(analy[sz]e|research|investigate|summari[sz]e|review|audit)\b/,
  /\b(fix|debug|refactor|implement)\b/,
  /\b(find|look up|gather)\b/,
  /\bonce\b/,
  /\bone[- ]off\b/,
  /\bright now\b/,
  /\ba report on\b/,
];

export function classifyIntent(raw: string): IntentClassification {
  const text = (raw ?? "").toLowerCase();
  if (text.trim().length === 0) {
    return { kind: "mission", confidence: "low", reason: "Describe the work to get a recommendation." };
  }

  const teamHits = TEAM_PATTERNS.filter((re) => re.test(text)).length;
  const missionHits = MISSION_PATTERNS.filter((re) => re.test(text)).length;

  if (teamHits > 0 && teamHits >= missionHits) {
    const confidence = teamHits >= 2 ? "high" : missionHits === 0 ? "medium" : "low";
    return {
      kind: "team",
      confidence,
      reason:
        "This reads as continuous, standing work (monitoring, a cadence, or multiple roles) — a team keeps running under a charter.",
    };
  }

  const confidence = missionHits >= 2 ? "high" : missionHits === 1 ? "medium" : "low";
  return {
    kind: "mission",
    confidence,
    reason:
      "This reads as a focused, one-off deliverable — a single mission composes, runs, and delivers, then it's done.",
  };
}
