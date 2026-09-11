import type { MissionArtifact, TaskDetail, TeamDetail, TeamRole } from "./types";

export interface CollaborationEvent {
  at: string | null;
  event: string;
  agent: string | null;
  member: string | null;
  outcome: string | null;
  message_id: string | null;
  preview: string | null;
  source: "ledger" | "router" | "harness" | "governance" | "agent-reported" | "artifact-derived";
}

export interface ResearchEvent {
  at: string | null;
  agent: string | null;
  url: string;
  host: string | null;
  outcome: string | null;
  status: number | null;
  digest: string | null;
  source: "router" | "agent-reported";
}

export interface RoleEvidence {
  role: TeamRole;
  state: "delivered" | "skipped" | "working" | "missing" | "failed";
  artifacts: MissionArtifact[];
  handbacks: CollaborationEvent[];
  artifactAttribution: "recorded" | "inferred" | "none";
}

export interface TeamRunEvidence {
  outcome: "paused" | "running" | "delivered" | "delivered_with_issues" | "incomplete" | "failed";
  roles: RoleEvidence[];
  collaboration: CollaborationEvent[];
  research: ResearchEvent[];
  issues: string[];
  evidenceMode: "ledger" | "agent-reported" | "artifact-derived";
  unattributedArtifacts: MissionArtifact[];
}

export type TeamEvidenceInput = Pick<TeamDetail, "roster" | "paused">;
export type TaskEvidenceInput = Pick<
  TaskDetail,
  | "artifacts"
  | "activity"
  | "result"
  | "launched"
  | "execution_phase"
  | "assignment"
  | "assignment_events"
  | "current_run_nonce"
  | "role_plan"
  | "collaboration_events"
>;

function parseJsonLines<T>(artifact: MissionArtifact | undefined): T[] {
  if (!artifact?.content || artifact.content_truncated) return [];
  return artifact.content
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .flatMap((line) => {
      try {
        return [JSON.parse(line) as T];
      } catch {
        return [];
      }
    });
}

function humanEvidencePreview(value: unknown): string | null {
  if (typeof value !== "string" || !value.trim()) return null;
  const normalized = value.replace(/\\"/g, "\"");
  if (normalized.includes("\"type\":\"file_transfer\"")) {
    const file = normalized.match(/"file_name"\s*:\s*"([^"]+)"/)?.[1] ?? "artifact";
    const size = normalized.match(/"size_bytes"\s*:\s*(\d+)/)?.[1];
    return `Transferred ${file}${size ? ` (${Number(size).toLocaleString()} bytes)` : ""} into the recipient workspace.`;
  }
  try {
    const parsed = JSON.parse(normalized) as Record<string, unknown>;
    if (parsed.type === "file_transfer") {
      const file = typeof parsed.file_name === "string" ? parsed.file_name : "artifact";
      const size = typeof parsed.size_bytes === "number"
        ? ` (${parsed.size_bytes.toLocaleString()} bytes)`
        : "";
      return `Transferred ${file}${size} into the recipient workspace.`;
    }
    if (typeof parsed.message === "string") return parsed.message;
  } catch {
    // Plain-text evidence is already suitable for display.
  }
  return value.replace(/\s+/g, " ").trim();
}

function roleScore(role: TeamRole, artifact: MissionArtifact): number {
  const normalize = (value: string) => value.toLowerCase().replace(/[^a-z0-9]+/g, "");
  const roleId = normalize(role.name);
  if (!roleId) return 0;
  const provenance = [
    artifact.name,
    artifact.source_agent ?? "",
    artifact.source_path ?? "",
  ].map(normalize);
  if (normalize(artifact.source_agent ?? "") === roleId) return 110;
  if (provenance.some((value) => value.includes(roleId))) return 100;
  const roleText = `${role.name} ${role.system_prompt ?? ""}`.toLowerCase();
  const artifactText = `${artifact.name} ${artifact.source_path ?? ""}`.toLowerCase();
  if (
    /(test|quality|verification|qa)/.test(roleText) &&
    /(test|spec|verification|validation|test_report)/.test(artifactText)
  ) {
    return 80;
  }
  if (
    /(ux|browser|design|usability|visual)/.test(roleText) &&
    /(screenshot|ux|playwright|browser|\.(png|jpg|jpeg|svg|pdf)$)/.test(artifactText)
  ) {
    return 80;
  }
  if (
    /(application|implement|build|developer|engineer|source)/.test(roleText) &&
    /(readme|server|app|style|index|stock|requirement|source|dashboard|\.(py|js|mjs|ts|tsx|jsx|html|css|json|zip|tgz|tar\.gz)$)/.test(artifactText)
  ) {
    return 60;
  }
  return 0;
}

function rolePlan(task: TaskEvidenceInput): {
  selected: Set<string>;
  skipped: Set<string>;
} {
  return {
    selected: new Set(task.role_plan.selected_roles),
    skipped: new Set(task.role_plan.skipped_roles),
  };
}

export function analyzeTeamRun(team: TeamEvidenceInput, task: TaskEvidenceInput): TeamRunEvidence {
  const currentTaskId = task.current_run_nonce ?? task.assignment?.task_id ?? null;
  const currentAssignment =
    task.assignment != null
    && (currentTaskId == null || task.assignment.task_id === currentTaskId)
      ? task.assignment
      : null;
  const currentAssignmentEvents = task.assignment_events.filter(
    (event) => currentTaskId == null || event.task_id === currentTaskId,
  );
  const collaborationArtifact = task.artifacts.find(
    (a) =>
      a.name === "collaboration.jsonl" ||
      a.source_path?.endsWith("/collaboration.jsonl"),
  );
  const rawCollaboration: Record<string, unknown>[] = task.collaboration_events.length > 0
    ? task.collaboration_events.map((event) => ({ ...event }))
    : parseJsonLines<Record<string, unknown>>(collaborationArtifact);
  const ledgerCollaboration: CollaborationEvent[] = currentAssignmentEvents.map((event) => ({
    at: event.at,
    event: event.stage ?? event.event_type,
    agent: event.worker_did,
    member: event.child_role,
    outcome: event.outcome ?? event.state,
    message_id: event.child_task_id ?? event.task_id,
    preview: event.message,
    source: "ledger",
  }));
  const collaboration: CollaborationEvent[] = [
    ...ledgerCollaboration,
    ...rawCollaboration.map((e) => ({
    at: typeof e.at === "string" ? e.at : null,
    event: typeof e.event === "string" ? e.event : "event",
    agent: typeof e.agent === "string" ? e.agent : null,
    member: typeof e.member === "string"
      ? e.member
      : typeof e.from_agent === "string"
        ? e.from_agent
        : typeof e.to_agent === "string"
          ? e.to_agent
          : null,
    outcome: typeof e.outcome === "string" ? e.outcome : null,
    message_id: typeof e.message_id === "string" ? e.message_id : null,
    preview: humanEvidencePreview(
      typeof e.reply_preview === "string"
        ? e.reply_preview
        : typeof e.content_preview === "string"
          ? e.content_preview
          : null,
    ),
    source: "agent-reported" as const,
    })),
  ];
  collaboration.push(
    ...task.activity.flatMap((event) => {
      if (
        event.kind !== "tool" ||
        !["router", "harness", "governance"].includes(event.source ?? "") ||
        event.name === "http_fetch"
      ) {
        return [];
      }
      return [{
        at: event.ts || null,
        event: "mcp_tool_call",
        agent: event.agent ?? null,
        member: null,
        outcome: event.ok ? "success" : "failed",
        message_id: null,
        preview: `${event.name}${event.args_preview ? ` · ${event.args_preview}` : ""} · ${event.result_preview || "no result preview"}`,
        source: (event.source ?? "router") as "router" | "harness" | "governance",
      }];
    }),
  );

  const research: ResearchEvent[] = task.activity.flatMap((event) => {
      if (event.kind !== "tool" || event.name !== "http_fetch" || event.source !== "router") return [];
      const statusMatch = event.result_preview.match(/HTTP\s+(\d{3})/i);
      let host: string | null = null;
      try {
        host = new URL(event.args_preview).host;
      } catch {
        host = null;
      }
      return [{
        at: event.ts || null,
        agent: event.agent ?? null,
        url: event.args_preview,
        host,
        outcome: event.ok ? "success" : "failed",
        status: statusMatch ? Number(statusMatch[1]) : null,
        digest: null,
        source: "router",
      }];
    });

  const evidenceArtifacts = task.artifacts.filter(
    (a) =>
      !a.name.endsWith("collaboration.jsonl") &&
      !a.name.endsWith("research-evidence.jsonl"),
  );
  const assigned = new Map<string, { role: string; inferred: boolean }>();
  for (const artifact of evidenceArtifacts) {
    const ranked = team.roster
      .map((role) => ({ role: role.name, score: roleScore(role, artifact) }))
      .filter((r) => r.score > 0)
      .sort((a, b) => b.score - a.score);
    if (ranked[0]) {
      assigned.set(artifact.name, {
        role: ranked[0].role,
        inferred: ranked[0].score < 100,
      });
    }
  }
  const plan = rolePlan(task);
  const hasExplicitPlan = plan.selected.size > 0 || plan.skipped.size > 0;
  const hasSelectedSet = plan.selected.size > 0;
  const normalizeRole = (value: string) => value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  const memberMatchesRole = (member: string, role: string) => {
    const normalizedMember = normalizeRole(member);
    const normalizedRole = normalizeRole(role);
    return normalizedMember === normalizedRole
      || normalizedMember.endsWith(`-${normalizedRole}`);
  };
  const roles: RoleEvidence[] = team.roster.map((role) => {
    const artifacts = evidenceArtifacts.filter((a) => assigned.get(a.name)?.role === role.name);
    const artifactAttribution = artifacts.length === 0
      ? "none"
      : artifacts.some((artifact) => assigned.get(artifact.name)?.inferred)
        ? "inferred"
        : "recorded";
    const roleEvents = ledgerCollaboration.filter(
      (event) =>
        event.member != null &&
        memberMatchesRole(event.member, role.name),
    );
    const failedOutcome = (event: CollaborationEvent) =>
      event.outcome === "failed" || event.outcome === "Failed";
    const handbacks = roleEvents.filter(
      (event) =>
        (event.event === "child_handback" && !failedOutcome(event)) ||
        event.outcome === "success" ||
        event.outcome === "Completed",
    );
    const failed = handbacks.length === 0 && roleEvents.some(
      (event) =>
        event.event === "child_lease_expired" ||
        failedOutcome(event),
    );
    const lifecycleAssigned = roleEvents.some(
      (event) =>
        event.event === "child_assigned" ||
        event.event === "child_progress" ||
        event.event === "child_handback" ||
        event.event === "child_lease_expired",
    );
    return {
      role,
      state: plan.skipped.has(role.name) || (hasSelectedSet && !plan.selected.has(role.name))
        ? "skipped"
        : handbacks.length > 0
          ? "delivered"
          : failed
            ? "failed"
            : lifecycleAssigned && task.result == null
              ? "working"
              : "missing",
      artifacts,
      handbacks,
      artifactAttribution,
    };
  });

  if (collaboration.length === 0) {
    for (const role of roles.filter((r) => r.artifacts.length > 0)) {
      collaboration.push({
        at: task.result?.finished_at ?? null,
        event: "role_artifact_recovered",
        agent: null,
        member: role.role.name,
        outcome: "delivered",
        message_id: null,
        preview: `${role.artifacts.length} retained artifact${role.artifacts.length === 1 ? "" : "s"}`,
        source: "artifact-derived",
      });
    }
  }

  const issues: string[] = [];
  if (task.result?.artifact_persistence === "partial") {
    issues.unshift(
      `Only ${task.result.artifact_count ?? 0} of ${task.result.declared_artifact_count ?? "the declared"} artifacts were durably persisted.`,
    );
  }
  const missingRoles = roles.some(
    (role) =>
      (role.state === "missing" || role.state === "failed") &&
      (!hasExplicitPlan || plan.selected.has(role.role.name)),
  );
  const partialArtifacts = task.result?.artifact_persistence === "partial";
  const awaitingAssignment = Boolean(
    task.current_run_nonce
    && task.assignment?.task_id !== task.current_run_nonce,
  );
  const resultMatchesCurrentRun =
    task.result == null
    || task.current_run_nonce == null
    || task.result.assignment_nonce == null
    || task.result.assignment_nonce === task.current_run_nonce;
  const latestRootEvent = [...currentAssignmentEvents]
    .filter(
      (event) =>
        event.child_task_id == null
        && (currentTaskId == null || event.task_id === currentTaskId),
    )
    .sort((left, right) => right.sequence - left.sequence)[0];
  const assignmentState =
    currentAssignment?.state?.toLowerCase()
    ?? latestRootEvent?.state.toLowerCase()
    ?? null;
  const hasAssignmentLedger =
    currentAssignment != null || currentAssignmentEvents.length > 0;
  const rootCompleted = assignmentState === "completed";
  const rootFailed = assignmentState === "failed";
  const running =
    task.launched &&
    (task.result == null || !resultMatchesCurrentRun) &&
    !rootFailed &&
    (awaitingAssignment ||
      assignmentState === "assigned" ||
      assignmentState === "acknowledged" ||
      assignmentState === "running" ||
      task.execution_phase === "Running");
  const failed =
    rootFailed || (resultMatchesCurrentRun && task.result?.status === "error");
  const blocked = resultMatchesCurrentRun && Boolean(task.result?.blocked);
  const outcome = team.paused && running
    ? "paused"
    : running
      ? "running"
    : failed
      ? "failed"
      : blocked || !hasAssignmentLedger || !rootCompleted || missingRoles
        ? "incomplete"
        : partialArtifacts
        ? "delivered_with_issues"
        : "delivered";

  return {
    outcome,
    roles,
    collaboration,
    research,
    issues,
    evidenceMode: hasAssignmentLedger
      ? "ledger"
      : rawCollaboration.length > 0
        ? "agent-reported"
      : collaboration.some((event) => event.source === "router")
        ? "agent-reported"
        : "artifact-derived",
    unattributedArtifacts: evidenceArtifacts.filter((a) => !assigned.has(a.name)),
  };
}
