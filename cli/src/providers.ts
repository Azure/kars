/**
 * Provider contracts — TypeScript side.
 *
 * Mirrors `controller/src/providers/` and `inference-router/src/providers/`
 * but scoped to CLI/plugin concerns: reads `spec.agt.providers`, emits
 * feature flags into sandbox env vars, validates user input before
 * submission.
 *
 * **Phase 0 status:** type + helper definitions only. No plugin.ts
 * call-site migrations land here. Wiring happens in Phase 1 per
 * internal Phase 1 plan §7.
 *
 * Runtime-side implementations live in Rust (router + controller). The TS
 * side only needs to:
 *   - validate user-provided provider choices before they hit the CRD,
 *   - construct env vars for sandbox images based on the selection,
 *   - detect Null* provider use and require the dev-only label.
 */

export type ProviderKind = "vendored" | "agt" | "null";

export interface ProviderSelection {
  mesh: ProviderKind;
  policy: ProviderKind;
  audit: ProviderKind;
  signing: ProviderKind;
}

/**
 * Default provider selection — everything vendored (Phase 0 zero-behaviour-
 * change baseline). Callers override individual fields per tenant.
 */
export const DEFAULT_PROVIDER_SELECTION: ProviderSelection = Object.freeze({
  mesh: "vendored",
  policy: "vendored",
  audit: "vendored",
  signing: "vendored",
}) as ProviderSelection;

/**
 * Reject `"noop" | "disabled"` as spec strings — the CLI accepts only
 * the canonical `"null"` alias. Controller-side admission policy
 * accepts all three; we keep CLI strict to catch typos early.
 */
export function parseProviderKind(s: string): ProviderKind | null {
  switch (s.trim().toLowerCase()) {
    case "vendored":
      return "vendored";
    case "agt":
      return "agt";
    case "null":
      return "null";
    default:
      return null;
  }
}

/** Does the selection include any `null` provider? */
export function selectionHasNull(sel: ProviderSelection): boolean {
  return (
    sel.mesh === "null" ||
    sel.policy === "null" ||
    sel.audit === "null" ||
    sel.signing === "null"
  );
}

/**
 * Label used by the admission policy (VAP) to permit `null` providers.
 * `ci/no-null-provider-prod.sh` is the static mirror.
 */
export const DEV_ONLY_LABEL_KEY = "azureclaw.azure.com/dev-only";
export const DEV_ONLY_LABEL_VALUE = "true";

/**
 * Env vars passed to the sandbox image telling it which provider to use
 * for each capability. Router reads these at boot.
 */
export function selectionToEnv(sel: ProviderSelection): Record<string, string> {
  return {
    AZURECLAW_PROVIDER_MESH: sel.mesh,
    AZURECLAW_PROVIDER_POLICY: sel.policy,
    AZURECLAW_PROVIDER_AUDIT: sel.audit,
    AZURECLAW_PROVIDER_SIGNING: sel.signing,
  };
}

/**
 * Outage mode selected per `ClawSandbox.spec.agt.outageMode`.
 * See internal Phase 1 plan §1.3.
 */
export type OutageMode = "strict" | "cached-read" | "degraded-dev";

export function parseOutageMode(s: string): OutageMode | null {
  switch (s.trim().toLowerCase()) {
    case "strict":
      return "strict";
    case "cached-read":
    case "cachedread":
      return "cached-read";
    case "degraded-dev":
    case "degradeddev":
      return "degraded-dev";
    default:
      return null;
  }
}
