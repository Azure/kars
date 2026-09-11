// kars Bridge web — server-side BFF client.
//
// This module runs only on the Next.js server. It is the single place the
// web app reaches the BFF, so auth-cookie forwarding and error normalization
// live here once. Importing this from a Client Component is a build error by
// design (it has no "use client").

import { bffBaseUrl } from "./config";
import { cookies } from "next/headers";
import { SESSION_COOKIE, verifySession } from "./session-token";
import { ssoConfigured } from "./oidc-config";
import { parseCredentialContinuation, parseCredentialReview,
  type CredentialContinuation, type CredentialInput, type CredentialReview } from "./credential-review";
import type {
  Approval,
  CreateTaskRequest,
  Options,
  Receipt,
  SystemStatus,
  TaskDetail,
  TaskSummary,
} from "./types";

/** Liveness shape returned by the BFF `/healthz`. */
export interface BffHealth {
  status: string;
  service: string;
  version: string;
}

/** Readiness shape returned by the BFF `/readyz`. */
export interface BffReadiness {
  status: string;
  cluster_configured: boolean;
}

/** Result of probing the BFF — either reachable with a payload, or not. */
export type BffProbe<T> =
  | { reachable: true; data: T }
  | { reachable: false; error: string };

async function getJson<T>(path: string): Promise<BffProbe<T>> {
  const url = `${bffBaseUrl()}${path}`;
  try {
    const res = await fetch(url, {
      // BFF readiness must never be cached — it reflects live state.
      cache: "no-store",
      headers: { accept: "application/json" },
    });
    if (!res.ok) {
      return { reachable: false, error: `BFF responded ${res.status}` };
    }
    const data = (await res.json()) as T;
    return { reachable: true, data };
  } catch (err) {
    const message = err instanceof Error ? err.message : "unknown error";
    return { reachable: false, error: message };
  }
}

/** Probe BFF readiness (includes cluster-wiring status). */
export function probeReadiness(): Promise<BffProbe<BffReadiness>> {
  return getJson<BffReadiness>("/readyz");
}

/** Probe BFF liveness + version. */
export function probeHealth(): Promise<BffProbe<BffHealth>> {
  return getJson<BffHealth>("/healthz");
}

// ─── KarsTask API ──────────────────────────────────────────────────────────
// Server-only client functions for the task surface. All run in Server
// Components / Route Handlers; the browser never calls the cluster directly.

async function requestJson<T>(
  path: string,
  init?: RequestInit,
): Promise<T> {
  const res = await authenticatedBffFetch(path, init);
  if (!res.ok) {
    let code = `http_${res.status}`;
    let message = "";
    let credentialContinuation: CredentialContinuation | undefined;
    try {
      const body = await res.json();
      if (body?.error?.code) code = body.error.code;
      if (body?.error?.message) message = body.error.message;
      if (res.status === 409 && code === "conflict")
        credentialContinuation = parseCredentialContinuation(body?.error?.credentialContinuation);
    } catch {
      // non-JSON error body; keep the status-derived code
    }
    throw new BffError(code, res.status, message, credentialContinuation);
  }
  return (await res.json()) as T;
}

/** Authenticated raw BFF request for server actions that need non-JSON bodies
 * or custom response handling. This is the ONLY direct BFF fetch path. */
export async function authenticatedBffFetch(
  path: string,
  init?: RequestInit,
): Promise<Response> {
  const headers = new Headers(init?.headers);
  headers.set("accept", "application/json");
  if (init?.body != null && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  if (ssoConfigured()) {
    const token = (await cookies()).get(SESSION_COOKIE)?.value;
    if (!token || !(await verifySession(token))) {
      throw new BffError("unauthorized", 401, "A signed Bridge session is required.");
    }
    headers.set("x-kars-principal-token", token);
  }
  return fetch(`${bffBaseUrl()}${path}`, {
    cache: "no-store",
    ...init,
    headers,
  });
}

/** Error carrying the BFF's stable error code + HTTP status + message. */
export class BffError extends Error {
  #credentialContinuation?: CredentialContinuation;

  constructor(
    public readonly code: string,
    public readonly status: number,
    message = "",
    credentialContinuation?: CredentialContinuation,
  ) {
    super(message || code);
    this.name = "BffError";
    this.#credentialContinuation = credentialContinuation;
  }

  get credentialContinuation(): CredentialContinuation | undefined { return this.#credentialContinuation; }
}

/** List tasks in a namespace. */
export function listTasks(namespace: string): Promise<TaskSummary[]> {
  return requestJson<TaskSummary[]>(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks`,
  );
}

/** Fetch one task by name. */
export function getTask(
  namespace: string,
  name: string,
): Promise<TaskDetail> {
  return requestJson<TaskDetail>(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}`,
  );
}

/** Create a task. */
export function createTask(
  namespace: string,
  body: CreateTaskRequest,
): Promise<TaskDetail> {
  return requestJson<TaskDetail>(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks`,
    { method: "POST", body: JSON.stringify(body) },
  );
}

// ─── KarsTeam API (standing orgs) ────────────────────────────────────────────

/** List standing teams in a namespace. */
export function listTeams(
  namespace: string,
): Promise<import("./types").TeamSummary[]> {
  return requestJson<import("./types").TeamSummary[]>(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams`,
  );
}

/** Fetch one team by name (charter, org chart, watching status, history). */
export function getTeam(
  namespace: string,
  name: string,
): Promise<import("./types").TeamDetail> {
  return requestJson<import("./types").TeamDetail>(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(name)}`,
  );
}

export function getEngineeringSource(
  namespace: string,
  team: string,
): Promise<import("./types").EngineeringSource> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(team)}/engineering-source`,
  );
}

export function putEngineeringSource(
  namespace: string,
  team: string,
  body: {
    enabled: boolean;
    auto_run: boolean;
    repos: string[];
    signals: import("./types").EngineeringSignal[];
    poll_interval_seconds: number;
  },
): Promise<import("./types").EngineeringSource> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(team)}/engineering-source`,
    { method: "PUT", body: JSON.stringify(body) },
  );
}

export function syncEngineeringSource(
  namespace: string,
  team: string,
): Promise<import("./types").EngineeringSource> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(team)}/engineering-source/sync`,
    { method: "POST" },
  );
}

export function deleteEngineeringSource(
  namespace: string,
  team: string,
): Promise<import("./types").EngineeringSource> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(team)}/engineering-source`,
    { method: "DELETE" },
  );
}

export function getGithubConnection(
  namespace: string,
): Promise<import("./types").GithubConnection> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/github/connection`,
  );
}

/** Fetch a team's knowledge commons (shared, provenance-tracked memory). */
export function getTeamCommons(
  namespace: string,
  name: string,
): Promise<import("./types").CommonsResponse> {
  return requestJson<import("./types").CommonsResponse>(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(name)}/commons`,
  );
}

export function getTeamChannels(
  namespace: string,
  name: string,
): Promise<import("./types").TeamChannelsState> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(name)}/channels`,
  );
}

export function getArchivedTeamRun(
  namespace: string,
  team: string,
  run: string,
): Promise<import("./types").CommonsEntry> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(team)}/runs/${encodeURIComponent(run)}/archive`,
  );
}

// ─── Artifact review (§16) ───────────────────────────────────────────────────

/** Fetch the review state for a mission's deliverable. */
export function getReview(
  namespace: string,
  name: string,
): Promise<import("./types").ReviewState> {
  return requestJson<import("./types").ReviewState>(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/review`,
  );
}

/** Record a review decision; request_changes re-drives the producing task. */
export function postReview(
  namespace: string,
  name: string,
  body: { decision: "approve" | "request_changes"; comment?: string },
): Promise<import("./types").ReviewState> {
  return requestJson<import("./types").ReviewState>(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/review`,
    { method: "POST", body: JSON.stringify(body) },
  );
}

/** Request a temporary, scoped egress grant for a mission's sandbox. Files an
 * EgressApproval; widens nothing until a human approves it. */
export function requestEgress(
  namespace: string,
  name: string,
  body: { host: string; port?: number | null; reason: string; ttl?: string },
): Promise<{ requested: boolean; name: string | null; note: string }> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/egress`,
    { method: "POST", body: JSON.stringify(body) },
  );
}

/** The cross-team digest stream (§20), newest first. */
export function getDigests(): Promise<import("./types").Digest[]> {
  return requestJson<import("./types").Digest[]>("/api/digests");
}

/** The cross-harness efficiency frontier (§3B). */
export function getEfficiency(): Promise<import("./types").Efficiency> {
  return requestJson<import("./types").Efficiency>("/api/efficiency");
}

/** The hierarchical inference-budget config + live measured daily usage. */
export function getInferenceBudgets(): Promise<import("./types").InferenceBudgets> {
  return requestJson<import("./types").InferenceBudgets>("/api/operator/inference-budgets");
}

/** Cluster-wide mission/team-run retention default (auto-delete delivered
 *  records after a TTL — mirrors Kubernetes' Job.ttlSecondsAfterFinished). */
export function getRetentionPolicy(): Promise<import("./types").RetentionPolicy> {
  return requestJson<import("./types").RetentionPolicy>("/api/operator/retention-policy");
}

/** Set the cluster-wide retention default (admin-only, enforced server-side). */
export function setRetentionPolicy(
  defaultTtlSeconds: number,
): Promise<import("./types").RetentionPolicy> {
  return requestJson<import("./types").RetentionPolicy>("/api/operator/retention-policy", {
    method: "PUT",
    body: JSON.stringify({ default_ttl_seconds: defaultTtlSeconds }),
  });
}

/** The team's continuous ledger (§14), newest first. */
export function getTeamLedger(
  namespace: string,
  name: string,
): Promise<import("./types").LedgerEvent[]> {
  return requestJson<import("./types").LedgerEvent[]>(
    `/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(name)}/ledger`,
  );
}

/** Fetch the composable launch-package options from live cluster state. */
export function getOptions(): Promise<Options> {
  return requestJson<Options>("/api/options");
}

/** Validate a launch package against live cluster state (the §20 gate). */
export function validatePackage(
  namespace: string,
  blueprint: unknown,
  envelope?: { tier?: number; budget_tokens?: number | null; workload?: "mission" | "team" },
): Promise<import("./types").ValidationResult> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/validate`,
    { method: "POST", body: JSON.stringify({
      blueprint,
      tier: envelope?.tier,
      budget_tokens: envelope?.budget_tokens ?? undefined,
      workload: envelope?.workload,
    }) },
  );
}

/** Ask the orchestrator to compose a launch package from a plain objective. */
export function composePackage(
  namespace: string,
  objective: string,
): Promise<import("./types").ComposeResponse> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/compose`,
    {
      method: "POST",
      body: JSON.stringify({ objective }),
      signal: AbortSignal.timeout(120_000),
    },
  );
}

/** Ask the orchestrator to propose a 2026 loop (pattern + goal + criteria) from
 *  a raw intent, for the user to review in the Loop Designer before executing. */
export function proposeLoop(
  namespace: string,
  intent: string,
  surface: "mission" | "team",
): Promise<import("./types").LoopProposal> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/propose-loop`,
    {
      method: "POST",
      body: JSON.stringify({ intent, surface }),
      signal: AbortSignal.timeout(120_000),
    },
  );
}

/** Ask the orchestrator to compose an org chart from a team charter. */
export function composeTeam(
  namespace: string,
  charter: string,
): Promise<import("./types").ComposeTeamResponse> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/compose-team`,
    {
      method: "POST",
      body: JSON.stringify({ charter }),
      // Team composition can perform one structural repair and one qualification
      // repair after the initial proposal. Keep the browser alive for that bounded
      // server workflow instead of aborting a valid final repair mid-flight.
      signal: AbortSignal.timeout(300_000),
    },
  );
}

/** Independently verify a receipt's DSSE/Ed25519 signature server-side against
 *  the controller's published public-key anchor — a real cryptographic verdict
 *  in the browser, no CLI required. */
export function verifyReceipt(
  namespace: string,
  task: string,
): Promise<import("./types").VerifyResult> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(task)}/receipt/verify`,
    { method: "POST" },
  );
}

/** The cross-mission deliverable index — real captured artifacts. */
export function getArtifacts(): Promise<import("./types").ArtifactsIndex> {
  return requestJson<import("./types").ArtifactsIndex>("/api/artifacts");
}

/** Fetch the Governance Receipt for a task, or null if it has none. */
export async function getReceipt(
  namespace: string,
  name: string,
): Promise<Receipt | null> {
  try {
    return await requestJson<Receipt>(
      `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/receipt`,
    );
  } catch (e) {
    if (e instanceof BffError && e.status === 404) return null;
    throw e;
  }
}

/** Fetch the honest system / wiring status. */
export function getSystem(): Promise<SystemStatus> {
  return requestJson<SystemStatus>("/api/system");
}

/** List KarsEval safety/conformance evals with their latest verdicts. */
export function listEvals(): Promise<import("./types").Eval[]> {
  return requestJson<import("./types").Eval[]>("/api/operator/evals");
}

/** Fetch the compliance evidence pack (EU AI Act / NIST AI RMF) derived from a
 *  mission's signed receipt, or null if it has no receipt yet. */
export async function getCompliancePack(
  namespace: string,
  name: string,
): Promise<import("./types").CompliancePack | null> {
  try {
    return await requestJson<import("./types").CompliancePack>(
      `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/compliance`,
    );
  } catch (e) {
    if (e instanceof BffError && e.status === 404) return null;
    throw e;
  }
}

export function getDiagnostics(): Promise<import("./types").Diagnostics> {
  return requestJson("/api/operator/diagnostics");
}

export function getOrchestrator(): Promise<import("./types").Orchestrator> {
  return requestJson("/api/operator/orchestrator");
}

export function getIntegrations(): Promise<import("./types").Integrations> {
  return requestJson("/api/operator/integrations");
}

export function getGithubApp(): Promise<import("./types").GithubApp> {
  return requestJson("/api/github/app");
}

// ─── kars-SRE self-remediation proposals ────────────────────────────────────

export function listSreActions(): Promise<import("./types").SreAction[]> {
  return requestJson("/api/operator/sre-actions");
}

export function decideSreAction(
  namespace: string,
  name: string,
  body: { verdict: "approve" | "reject"; note?: string },
): Promise<import("./types").SreAction> {
  return requestJson(
    `/api/operator/sre-actions/${encodeURIComponent(namespace)}/${encodeURIComponent(name)}/decision`,
    { method: "POST", body: JSON.stringify(body) },
  );
}

// ─── Steering / HITL approvals ───────────────────────────────────────────────

/** Fleet-wide steering inbox. Pass `pending` to show only undecided. */
export function listApprovals(
  namespace: string,
  opts?: { pending?: boolean; scopeAll?: boolean },
): Promise<Approval[]> {
  const params = new URLSearchParams();
  if (opts?.pending) params.set("pending", "true");
  if (opts?.scopeAll) params.set("scope_all", "true");
  const q = params.size > 0 ? `?${params}` : "";
  return requestJson<Approval[]>(
    `/api/namespaces/${encodeURIComponent(namespace)}/approvals${q}`,
  );
}

/** Approvals gating a single task. */
export function listTaskApprovals(
  namespace: string,
  name: string,
): Promise<Approval[]> {
  return requestJson<Approval[]>(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/approvals`,
  );
}

/** Record a human decision (approve/deny) on an approval. */
export function decideApproval(
  namespace: string,
  name: string,
  body: {
    verdict: "approve" | "deny";
    reason?: string;
    resource_version: string;
    bound_envelope_digest: string | null;
  },
): Promise<Approval> {
  return requestJson<Approval>(
    `/api/namespaces/${encodeURIComponent(namespace)}/approvals/${encodeURIComponent(name)}/decision`,
    { method: "POST", body: JSON.stringify(body) },
  );
}

// ─── Operator Console + Insights (real cluster reads) ───────────────────────

export function listSandboxes(): Promise<import("./types").Sandbox[]> {
  return requestJson("/api/operator/sandboxes");
}
export function getClusterCapacity(): Promise<import("./types").ClusterCapacity> {
  return requestJson("/api/operator/capacity");
}
export function listMcpServers(): Promise<import("./types").McpServer[]> {
  return requestJson("/api/operator/mcpservers");
}
export function listToolPolicies(): Promise<import("./types").ToolPolicy[]> {
  return requestJson("/api/operator/toolpolicies");
}
export function listInferencePolicies(): Promise<import("./types").InferencePolicy[]> {
  return requestJson("/api/operator/inferencepolicies");
}
export function listEgress(): Promise<import("./types").EgressApproval[]> {
  return requestJson("/api/operator/egress");
}
export function getDatapathWitness(): Promise<import("./types").DatapathWitness> {
  return requestJson("/api/operator/datapath-witness");
}
export function getInsights(): Promise<import("./types").Insights> {
  return requestJson("/api/insights");
}
export function getScorecard(
  namespace: string,
  name: string,
): Promise<import("./types").Scorecard> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/scorecard`,
  );
}

/** Live, cluster-backed troubleshooting for a failed run: real pod/container
 *  status + the agent's own log tail + an evidence-derived diagnosis. */
export function getTroubleshoot(
  namespace: string,
  name: string,
): Promise<import("./types").Troubleshoot> {
  return requestJson(
    `/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/troubleshoot`,
  );
}

export function getAudit(): Promise<import("./types").Audit> {
  return requestJson("/api/operator/audit");
}

export function listSkills(): Promise<import("./types").SkillSummary[]> {
  return requestJson("/api/operator/skills");
}
/** User-side: skills visible to the user (same list, user framing). */
export function listUserSkills(): Promise<import("./types").SkillSummary[]> {
  return requestJson("/api/skills");
}
/** Fleet-wide live telemetry: aggregate live metrics + merged activity feed. */
export function getFleetTelemetry(): Promise<import("./types").FleetTelemetry> {
  return requestJson("/api/agents/fleet");
}
export interface SubmitSkillInput {
  name: string;
  display_name?: string;
  version: string;
  summary: string;
  bounding_policy: string;
  recipe?: string;
  mcp_servers?: string[];
  uploaded_by?: string;
  /** Package files — flat filenames (SKILL.md + scripts) the agent installs. */
  files?: { name: string; content: string }[];
}
/** User-side: submit a skill package. It lands PENDING operator review. */
export function submitSkill(
  input: SubmitSkillInput,
): Promise<import("./types").SkillSummary> {
  return requestJson("/api/skills", {
    method: "POST",
    body: JSON.stringify(input),
  });
}
export function listProfiles(): Promise<import("./types").ProfileSummary[]> {
  return requestJson("/api/operator/profiles");
}
export function putCredential(body: CredentialInput & { value: string; review?: string }): Promise<{ stored: boolean; source: { name: string; uid: string }; namespace: string; phase: string; note: string }> {
  return requestJson("/api/operator/credentials", { method: "POST", body: JSON.stringify(body) });
}

export async function reviewCredential(body: CredentialInput & { continuation?: string }): Promise<CredentialReview> {
  const result = await requestJson<unknown>("/api/operator/credentials/review", { method: "POST", body: JSON.stringify(body) });
  const review = parseCredentialReview(result);
  if (!review) throw new BffError("invalid_credential_review", 502, "Credential review metadata was malformed.");
  return review;
}

/** Configure the ONE shared kars GitHub App (operator self-service — replaces
 *  the manual `kubectl create secret` step). Verified against the real
 *  GitHub API before the credential is stored. */
export function putGithubApp(body: { app_id: string; private_key: string }): Promise<{ configured: boolean; slug: string | null; name: string | null; note: string }> {
  return requestJson("/api/operator/github-app", { method: "PUT", body: JSON.stringify(body) });
}

export function deleteGithubApp(): Promise<{ configured: boolean }> {
  return requestJson("/api/operator/github-app", { method: "DELETE" });
}

/** Author or edit a governance CRD (ToolPolicy / McpServer / KarsSkill) via the
 *  BFF's Server-Side Apply endpoint — create on first apply, edit on re-apply. */
export function applyGovernance(
  plural: "toolpolicies" | "mcpservers" | "skills" | "profiles" | "inferencepolicies",
  body: { name: string; spec: unknown; namespace?: string; force?: boolean },
): Promise<{ applied: boolean; kind: string; name: string; namespace: string; note: string }> {
  return requestJson(`/api/operator/${plural}`, { method: "PUT", body: JSON.stringify(body) });
}

/** Delete an operator-authored governance object (or revoke an egress grant). */
export function deleteGovernance(
  plural: "toolpolicies" | "mcpservers" | "skills" | "profiles" | "egress" | "inferencepolicies",
  name: string,
): Promise<{ deleted: boolean; kind: string; name: string; namespace: string; note: string }> {
  return requestJson(`/api/operator/${plural}/${encodeURIComponent(name)}`, { method: "DELETE" });
}

/** Approve + version-lock a skill (operator trust gate). Users only see
 * approved+locked skills. */
export function approveSkill(
  name: string,
  approved_by?: string,
): Promise<import("./types").SkillSummary> {
  return requestJson(`/api/operator/skills/${encodeURIComponent(name)}/approve`, {
    method: "POST",
    body: JSON.stringify({ approved_by: approved_by ?? null }),
  });
}

/** Revoke a skill's approval, returning it to review. */
export function revokeSkill(name: string): Promise<import("./types").SkillSummary> {
  return requestJson(`/api/operator/skills/${encodeURIComponent(name)}/revoke`, { method: "POST" });
}

/** Replicate a mission's exact package k times to measure pass^k reliability. */
/** Request a per-mission tier promotion (§12). */
export function promoteMission(
  namespace: string,
  name: string,
  tier: number,
): Promise<{ requested: boolean; tier: number; note: string }> {
  return requestJson(`/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/promote`, {
    method: "POST",
    body: JSON.stringify({ tier }),
  });
}

export function replicateMission(
  namespace: string,
  name: string,
  count: number,
): Promise<{ replicated: string; count: number; runs: string[]; note: string }> {
  return requestJson(`/api/namespaces/${encodeURIComponent(namespace)}/tasks/${encodeURIComponent(name)}/replicate`, {
    method: "POST",
    body: JSON.stringify({ count, launch: true }),
  });
}

/** Operator: list/upsert/delete MCP profiles (vetted McpServer bundles). */
export function putMcpProfile(body: { name: string; summary?: string | null; servers: string[] }): Promise<import("./types").McpProfileOption[]> {
  return requestJson("/api/operator/mcp-profiles", { method: "PUT", body: JSON.stringify(body) });
}
export function deleteMcpProfile(name: string): Promise<import("./types").McpProfileOption[]> {
  return requestJson(`/api/operator/mcp-profiles/${encodeURIComponent(name)}`, { method: "DELETE" });
}

/** Operator: Foundry connection status/onboarding. */
export interface FoundryStatus {
  connected: boolean;
  project_endpoint: string | null;
  inference_endpoint: string | null;
  memory_store_id: string | null;
  auth: string | null;
  has_api_key: boolean;
}
export interface FoundryCheck { label: string; status: string; detail: string }
export interface FoundryConnection { name: string; category: string | null }
export interface FoundryDiscovered {
  models: string[];
  connections: FoundryConnection[];
  memory_store_found: boolean | null;
}
export interface FoundryVerifyResult { checks: FoundryCheck[]; discovered: FoundryDiscovered }

export function getFoundry(): Promise<FoundryStatus> {
  return requestJson("/api/operator/foundry");
}
export function connectFoundry(body: {
  project_endpoint: string;
  inference_endpoint?: string;
  memory_store_id?: string;
  auth: "api" | "managed-identity";
  api_key?: string;
}): Promise<{ connected: boolean; note: string }> {
  return requestJson("/api/operator/foundry", { method: "POST", body: JSON.stringify(body) });
}
export function disconnectFoundry(): Promise<{ connected: boolean; note: string }> {
  return requestJson("/api/operator/foundry", { method: "DELETE" });
}
export function verifyFoundry(): Promise<FoundryVerifyResult> {
  return requestJson("/api/operator/foundry/verify", { method: "POST" });
}

export function listAgents(): Promise<import("./types").AgentLifecycle[]> {
  return requestJson("/api/agents");
}

export interface CreateRole { name: string; system_prompt?: string; runtime?: string; model?: string; skills?: string[] }
export function createTeam(namespace: string, body: { name: string; display_name?: string; charter: string; tier?: number; authority_ceiling?: number; delegation_depth?: number; reporting_to?: string; knowledge_commons?: string; memory?: string; tool_policy?: string; runtime?: string; model?: string; model_fallbacks?: string[]; mcp_servers?: string[]; egress?: { host: string; port?: number }[]; egress_mode?: "learning" | "strict"; cadence_minutes?: number; lifecycle_mode?: import("./types").TeamLifecycleMode; warm_idle_seconds?: number; launch?: boolean; roles?: CreateRole[]; execution_plan?: import("./types").ExecutionPlan; git_write_repos?: string[]; created_by?: string }): Promise<{ created: boolean; name: string }> {
  return requestJson(`/api/namespaces/${encodeURIComponent(namespace)}/teams`, { method: "POST", body: JSON.stringify(body) });
}
export function updateTeam(namespace: string, name: string, body: { charter?: string; paused?: boolean; cadence_minutes?: number; reporting_to?: string; lifecycle_mode?: import("./types").TeamLifecycleMode; warm_idle_seconds?: number; runtime?: string; model?: string; model_fallbacks?: string[]; memory?: string; mcp_servers?: string[]; execution_plan?: import("./types").ExecutionPlan }): Promise<{ updated: boolean }> {
  return requestJson(`/api/namespaces/${encodeURIComponent(namespace)}/teams/${encodeURIComponent(name)}`, { method: "PATCH", body: JSON.stringify(body) });
}

export function putProvider(body: { kind: string; auth: string; endpoint?: string; models: string; key?: string }): Promise<{ onboarded: boolean; note: string }> {
  return requestJson("/api/operator/providers", { method: "POST", body: JSON.stringify(body) });
}

/** Live model discovery so the operator never hand-types a deployment id.
 *  Throws (BffError) when the kind has no live catalog to query (e.g. GitHub
 *  Copilot) or the round-trip to the provider fails. */
export function discoverModels(body: { kind: string; endpoint?: string; key?: string }): Promise<import("./types").DiscoveredModel[]> {
  return requestJson("/api/operator/providers/discover", { method: "POST", body: JSON.stringify(body) });
}

// ─── GitHub Copilot device-flow sign-in ─────────────────────────────────────
// Mints a Copilot-authorized token via GitHub's device flow (a stock `gh`
// token 404s on the Copilot exchange). The token is stored server-side; the
// browser only ever sees the user code + the discovered models.
export interface CopilotLoginStart { device_code: string; user_code: string; verification_uri: string; interval: number; expires_in: number }
export function copilotLoginStart(): Promise<CopilotLoginStart> {
  return requestJson("/api/operator/providers/copilot/login/start", { method: "POST" });
}
export interface CopilotLoginPoll { status: "pending" | "authorized"; models?: import("./types").DiscoveredModel[] }
export function copilotLoginPoll(device_code: string): Promise<CopilotLoginPoll> {
  return requestJson("/api/operator/providers/copilot/login/poll", { method: "POST", body: JSON.stringify({ device_code }) });
}

// ─── Additional providers (§ inference-provider-wizard) ─────────────────────
// Multiple providers can be configured at once (e.g. GitHub Copilot as the
// default, Azure AI Foundry also connected) — InferencePolicy decides which
// one a given sandbox's calls actually use, per request.

export function listAdditionalProviders(): Promise<import("./types").AdditionalProvider[]> {
  return requestJson("/api/operator/providers/additional");
}
export function putAdditionalProvider(body: { tag: string; endpoint?: string; api_key?: string; models: string }): Promise<{ configured: boolean; tag: string; note: string }> {
  return requestJson("/api/operator/providers/additional", { method: "PUT", body: JSON.stringify(body) });
}
export function deleteAdditionalProvider(tag: string): Promise<{ removed: boolean; tag: string }> {
  return requestJson(`/api/operator/providers/additional/${encodeURIComponent(tag)}`, { method: "DELETE" });
}
export function promoteAdditionalProvider(tag: string): Promise<{ promoted: boolean; tag: string; note: string }> {
  return requestJson(`/api/operator/providers/additional/${encodeURIComponent(tag)}/promote`, { method: "POST" });
}
export function setDefaultModel(deployment: string, provider: string): Promise<{ ok: boolean; default: string; provider: string }> {
  return requestJson("/api/operator/models/default", { method: "POST", body: JSON.stringify({ deployment, provider }) });
}


// ─── Local (in-cluster) inference (§ local-inference) ────────────────────────
// A model running entirely inside the cluster — no external API, no egress
// dependency. Built on AI Runway's ModelDeployment CRD, which kars does not
// install itself (see docs/local-inference.md in the kars core repo) — an
// operator installs AI Runway + KAITO once, the same tier as the GitHub App.

export interface LocalInferenceStatus {
  available: boolean;
  gpu_node_count: number;
  gpu_products: string[];
}
export function getLocalInferenceStatus(): Promise<LocalInferenceStatus> {
  return requestJson("/api/operator/local-inference/status");
}

export interface CuratedLocalModel {
  id: string;
  label: string;
  tier: "cpu" | "gpu";
  params: string;
}
export function getLocalInferenceCatalog(): Promise<CuratedLocalModel[]> {
  return requestJson("/api/operator/local-inference/catalog");
}

export interface LocalModelDeployment {
  name: string;
  namespace: string;
  managed: boolean;
  model_id: string | null;
  engine: string | null;
  provider: string | null;
  phase: string | null;
  message: string | null;
  endpoint: string | null;
  created_at: string | null;
}
export function listLocalModelDeployments(): Promise<LocalModelDeployment[]> {
  return requestJson("/api/operator/local-inference/deployments");
}
export function createLocalModelDeployment(body: { name: string; model_id: string; tier: "cpu" | "gpu"; image?: string; gpu_count?: number }): Promise<LocalModelDeployment> {
  return requestJson("/api/operator/local-inference/deployments", { method: "POST", body: JSON.stringify(body) });
}
export function deleteLocalModelDeployment(name: string): Promise<{ deleted: boolean; name: string }> {
  return requestJson(`/api/operator/local-inference/deployments/${encodeURIComponent(name)}`, { method: "DELETE" });
}

export interface DeployCondition { type: string; status: string; reason: string; message: string }
export interface DeployPodState { name: string; phase: string; ready: boolean; running: boolean; waiting_reason: string | null; waiting_message: string | null }
export interface DeployActivity { time: string | null; reason: string; message: string; type: string; count: number }
export interface LocalDeployLiveStatus {
  name: string;
  found: boolean;
  phase: string | null;
  message: string | null;
  percent: number;
  ready: boolean;
  failed: boolean;
  failure_reason: string | null;
  failure_message: string | null;
  replicas_desired: number;
  replicas_ready: number;
  conditions: DeployCondition[];
  pods: DeployPodState[];
  activities: DeployActivity[];
}
export function getLocalDeploymentLiveStatus(name: string): Promise<LocalDeployLiveStatus> {
  return requestJson(`/api/operator/local-inference/deployments/${encodeURIComponent(name)}/status`);
}
