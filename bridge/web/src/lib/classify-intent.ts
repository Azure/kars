// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
  /^\s*team\s*$/,
  /\bmonitor(ing|s)?\b/,
  /\bwatch(ing|es)?\b/,
  /\bcontinuous(ly)?\b/,
  /\bongoing\b/,
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
  const text = (raw ?? "").toLowerCase().replace(/[\u2010-\u2015]/g, "-");
  if (text.trim().length === 0) {
    return { kind: "mission", confidence: "low", reason: "Describe the work to get a recommendation." };
  }

  const affirmativeText = text.replace(
    /\b(?:not|no|without)\s+(?:(?:a|an)\s+)?(?:(?:standing|ongoing|persistent|recurring|recurrent|continuous|one-off|one-time|single-run)\s+)*(?:teams?|missions?|tasks?|reports?|assignments?|work)\b/g,
    "",
  );
  if (/\b(?:one[- ]off|one[- ]time|single[- ]run|bounded mission)\b/.test(affirmativeText)) {
    return {
      kind: "mission",
      confidence: "high",
      reason:
        "You asked for bounded, one-off work. A mission can use multiple agents and tools without becoming a standing team.",
    };
  }
  if (
    /^\s*(?:(?:please\s+)?(?:maintain|run|operate|establish|form|start|create|build|set up|organize|organise|(?:i|we)\s+(?:want|need))\s+)?(?:(?:a|an|the|our|my)\s+)?(?:standing|persistent|ongoing|continuous|recurring|recurrent)\b(?:[\s,-]+[\w-]+){0,4}[\s,-]+teams?\b/.test(affirmativeText)
  ) {
    return {
      kind: "team",
      confidence: "high",
      reason:
        "You asked for a standing team. Its reports and other deliverables remain work under a persistent charter, rather than turning it into a one-off mission.",
    };
  }
  const teamHits = TEAM_PATTERNS.filter((re) => re.test(affirmativeText)).length;
  const missionHits = MISSION_PATTERNS.filter((re) => re.test(text)).length;

  if (teamHits > 0 && teamHits >= missionHits) {
    const confidence = teamHits >= 2 ? "high" : missionHits === 0 ? "medium" : "low";
    return {
      kind: "team",
      confidence,
      reason:
        "This reads as ongoing work or an explicit team request. A standing team keeps working under a charter; use Mission for a bounded assignment, even when it needs multiple agents.",
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
