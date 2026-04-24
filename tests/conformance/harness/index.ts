/**
 * Harness placeholder — conformance test wiring lands alongside each
 * implementation PR. Phase 0 scaffold keeps this intentionally empty.
 *
 * Planned helpers (grow per phase, each behind its own PR):
 *   - Phase 0: loadSignalFixture(name), diffRatchetState(a, b)
 *   - Phase 1: kindCluster() — shared Kind handle for isolation e2e
 *   - Phase 1: providerAxis() — matrix runner over enabled MeshProviders
 *
 * This file is imported by specs so TypeScript resolves the path even
 * before helpers exist. Callers: none yet; specs are `it.todo` only.
 */

export const CONFORMANCE_HARNESS_VERSION = "phase0";
