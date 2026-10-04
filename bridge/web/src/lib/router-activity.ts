// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export const ROUTER_OBSERVATIONS_ARTIFACT = "kars-router-observations.json";

interface RouterEventIdentity {
  scope_id: string;
  round: number;
  seq: number;
}

export type RouterActivityEvent = RouterEventIdentity & (
  | {
    kind: "round";
    source: "router-upstream";
    provider: string;
    model: string | null;
    http_status: number | null;
    accepted: boolean | null;
    outcome: string;
    usage: {
      prompt_tokens: number | null;
      completion_tokens: number | null;
      total_tokens: number | null;
    } | null;
    usage_state: "present" | "partial" | "missing";
    finish_reason: string | null;
    partial_observation: boolean;
    ms: number;
    tool_calls_observed: number;
  }
  | { kind: "tool_proposed"; source: "model-proposed"; call_id: string; name: string; ok: null }
  | { kind: "tool_result"; source: "harness-reported"; call_id: string; name: string; ok: boolean | null; reported_in_round: number }
);

export type RouterActivity = {
  version: 1;
  source: "runtime-forwarded-router-observations";
  coverage: "returned-responses-only";
  responses: number;
  scope_id: string | null;
  rounds: number[];
} & (
  | { state: "unavailable"; reason: string }
  | {
    state: "observed";
    trace: {
      scope_id: string;
      rounds: number[];
      coverage: "returned-responses-only";
      durable: false;
      missing_rounds: number[];
      dropped_events: number;
      truncated: boolean;
      events: RouterActivityEvent[];
    };
  }
);

export function isSystemEvidenceArtifact(name: string): boolean {
  return name === ROUTER_OBSERVATIONS_ARTIFACT;
}
