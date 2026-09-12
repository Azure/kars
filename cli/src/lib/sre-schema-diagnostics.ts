// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { AsyncLocalStorage } from "node:async_hooks";
import { schemaSsaConflict, type SsaConflictFacts } from "./schema-ssa-conflicts.js";

const sources = {
  "helm-render": "core-helm-schemas",
  "helm-rollback-review": "core-helm-schemas",
  "helm-history-recheck": "core-helm-schemas",
  "schema-qualification": "sre-schema-migration",
  "registrar": "sre-schema-migration",
  "schema-inventory": "sre-schema-migration",
  "controller-quiescence": "sre-schema-migration",
  "schema-retention": "sre-schema-migration",
  "canonical-target": "sre-schema-migration",
  "canonical-before": "sre-schema-migration",
  "schema-owner": "sre-schema-migration",
  "stored-versions": "sre-schema-migration",
  "migration-recheck": "sre-schema-migration",
  "data-inventory": "sre-migration-data",
  "data-list-shape": "sre-migration-data",
  "data-item-identity": "sre-migration-data",
  "data-fields": "sre-migration-data",
  "data-server-validation": "sre-migration-data",
  "data-returned-identity": "sre-migration-data",
  "data-round-trip": "sre-migration-data",
  "data-recheck": "sre-migration-data",
  "new-authorities": "sre-migration-data",
  "schema-plan": "schema-stage",
  "policy-review": "schema-stage",
  "schema-plan-identity": "schema-stage",
  "helm-schema-match": "schema-stage",
  "schema-server-preview": "schema-stage",
  "schema-preview-identity": "schema-stage",
  "schema-write": "schema-stage",
  "schema-publication": "schema-stage",
} as const;
type Step = keyof typeof sources;
const kinds = new Set(["A2AAgent", "EgressApproval", "InferencePolicy", "KarsApproval", "KarsAuthConfig",
  "KarsEval", "KarsMemory", "KarsProfile", "KarsReceipt", "KarsSkill", "KarsSREAction", "KarsTask",
  "KarsTeam", "McpServer", "ToolPolicy", "TrustGraph", "KarsSandbox", "KarsPairing",
  "KarsBudgetAccount", "KarsCredentialGrant", "KarsSRERegistration", "Deployment"]);
const fields = new Set(["metadata/uid", "metadata/resourceVersion", "metadata/name", "metadata/namespace",
  "metadata/labels", "metadata/annotations", "metadata/ownerReferences", "metadata/finalizers", "spec", "status",
  "spec/envelope/budget/scope", "spec/defaultEnvelope/budget/scope", "spec/blueprint/credentialBindings",
  "spec/blueprint/githubBinding", "spec/roster/*/blueprint/credentialBindings", "spec/roster/*/blueprint/githubBinding",
  "spec/roster/*/envelope/budget/scope", "spec/managed", "spec/credentialsRef", "spec/credentialBindings",
  "spec/githubBinding", "spec/inferenceBudgetRef", "status/serviceObservation",
  "status/conditions/*/observedGeneration", "status/reportConfigMapRef", "status/reportConfigMapUid",
  "status/reportEvidenceDigest"]);
type Shape = "missing" | "empty" | "string" | "other";
interface Facts {
  step: Step;
  source: string;
  kind?: string;
  field?: string;
  shape?: Record<"uid" | "resourceVersion" | "kind" | "apiVersion", Shape>;
}
const traces = new AsyncLocalStorage<{ current: Facts }>();

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined;
}

function shape(value: unknown): Shape {
  return value === undefined ? "missing" : value === "" ? "empty" : typeof value === "string" ? "string" : "other";
}

export function schemaStep(step: Step, kind?: unknown, object?: unknown): void {
  const trace = traces.getStore();
  if (!trace) return;
  const value = record(object);
  const metadata = record(value?.metadata);
  trace.current = {
    step, source: `cli/src/lib/${sources[step]}.ts`,
    ...(typeof kind === "string" && kinds.has(kind) ? { kind } : {}),
    ...(value ? { shape: { uid: shape(metadata?.uid), resourceVersion: shape(metadata?.resourceVersion),
      kind: shape(value.kind), apiVersion: shape(value.apiVersion) } } : {}),
  };
}

export function schemaField(field: string): void {
  const trace = traces.getStore();
  if (trace) trace.current.field = fields.has(field) ? field : "unrecognized";
}

function failureCategory(error: unknown): { category: string; reason?: string } & Partial<SsaConflictFacts> {
  const value = record(error);
  const stderr = typeof value?.stderr === "string" ? value.stderr.slice(0, 16384) : "";
  const conflict = schemaSsaConflict(stderr);
  if (conflict) return { category: "api-rejection", reason: "Conflict", ...conflict };
  const reason = /^Error from server \((Forbidden|Unauthorized|Invalid|NotFound|AlreadyExists|Conflict|BadRequest|InternalError|ServiceUnavailable)\):/m.exec(stderr)?.[1];
  if (reason) return { category: "api-rejection", reason };
  if (value?.timedOut === true) return { category: "transport-timeout" };
  if (value?.code === "ENOENT") return { category: "missing-command" };
  if (typeof value?.exitCode === "number") return { category: "command-failure" };
  if (error instanceof SyntaxError) return { category: "invalid-json" };
  return { category: "local-check" };
}

/** Diagnostic scope only: no raw error/cause survives the CLI output boundary. */
export async function withSchemaPreparationDiagnostics<T>(run: () => Promise<T>): Promise<T> {
  const trace: { current: Facts } = { current: { step: "helm-render", source: "cli/src/lib/core-helm-schemas.ts" } };
  try {
    return await traces.run(trace, run);
  } catch (error) {
    const facts = { ...trace.current, ...failureCategory(error) };
    console.error(`SRE-SCHEMA-PREPARATION ${JSON.stringify(facts)}`);
    throw new Error(`SRE schema preparation failed at ${facts.step}: ${facts.reason ?? facts.category}`);
  }
}
