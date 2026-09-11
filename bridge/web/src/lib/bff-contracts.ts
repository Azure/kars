// Server-side BFF client contracts; transport and authentication remain in ./bff.


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

export interface CreateRole { name: string; system_prompt?: string; runtime?: string; model?: string; skills?: string[] }

// ─── GitHub Copilot device-flow sign-in ─────────────────────────────────────
// Mints a Copilot-authorized token via GitHub's device flow (a stock `gh`
// token 404s on the Copilot exchange). The token is stored server-side; the
// browser only ever sees the user code + the discovered models.
export interface CopilotLoginStart { device_code: string; user_code: string; verification_uri: string; interval: number; expires_in: number }
export interface CopilotLoginPoll { status: "pending" | "authorized"; models?: import("./types").DiscoveredModel[] }


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

export interface CuratedLocalModel {
  id: string;
  label: string;
  tier: "cpu" | "gpu";
  params: string;
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
