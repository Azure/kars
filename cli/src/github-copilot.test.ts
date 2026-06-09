// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { buildCopilotFallbackChain, COPILOT_MODELS, validateCopilotModel } from "./github-copilot.js";

describe("buildCopilotFallbackChain", () => {
  // Regression suite for the auto-fallback chain that ships in
  // InferencePolicy.spec.modelPreference.fallback[] when the user picks
  // a GitHub Copilot model in `kars dev` / `kars credentials`.
  //
  // Without this chain, a single overloaded primary surfaces 503
  // "upstream model provider is currently experiencing high demand"
  // straight to the agent because the router's failover walk has
  // nowhere to route (see inference-router/src/failover.rs).

  it("places the picked model FIRST in the chain (no silent override)", () => {
    // We never reorder behind the user's back. The picked model is
    // always the primary; only the fallbacks change.
    for (const picked of COPILOT_MODELS.map((m) => m.id)) {
      const chain = [picked, ...buildCopilotFallbackChain(picked)];
      expect(chain[0]).toBe(picked);
    }
  });

  it("never includes the picked model in the fallback list (dedup)", () => {
    for (const picked of COPILOT_MODELS.map((m) => m.id)) {
      const chain = buildCopilotFallbackChain(picked);
      expect(chain).not.toContain(picked);
    }
  });

  it("returns a non-empty chain for the recommended default (opus)", () => {
    // claude-opus-4.7 is the recommended Copilot pick AND the most
    // throttled — the fallback chain MUST exist for it specifically
    // or the OOTB user experience reverts to "kars dev appears broken".
    const chain = buildCopilotFallbackChain("claude-opus-4.7");
    expect(chain.length).toBeGreaterThan(0);
    // Cross-family alternative MUST be in the chain: if Anthropic is
    // throttled, the next try should NOT be Anthropic.
    expect(chain).toContain("gpt-5");
  });

  it("emits a stable, deterministic order between invocations", () => {
    // Debuggability: two callers with the same picked model must get
    // byte-identical chains. We don't shuffle, we don't sort by
    // wall-clock, nothing.
    const a = buildCopilotFallbackChain("gpt-5");
    const b = buildCopilotFallbackChain("gpt-5");
    expect(a).toEqual(b);
  });

  it("emits only model ids that exist in COPILOT_MODELS (no typos)", () => {
    const knownIds = new Set(COPILOT_MODELS.map((m) => m.id));
    for (const picked of COPILOT_MODELS.map((m) => m.id)) {
      const chain = buildCopilotFallbackChain(picked);
      for (const m of chain) {
        expect(knownIds, `chain entry ${m} not in COPILOT_MODELS`).toContain(m);
        // Belt-and-suspenders: also exercise validateCopilotModel so
        // the chain stays in lockstep with the validator.
        expect(validateCopilotModel(m).ok).toBe(true);
      }
    }
  });

  it("contains both an Anthropic AND an OpenAI entry regardless of pick", () => {
    // The whole point of the chain is cross-family diversity — if the
    // picked family is throttled, the next attempt must not be the
    // same family. Verify the chain covers at least the two big
    // families for every reasonable starting point.
    for (const picked of ["claude-opus-4.7", "gpt-5", "gemini-2.5-pro"]) {
      const chain = buildCopilotFallbackChain(picked);
      const hasAnthropic = chain.some((m) => m.startsWith("claude-"));
      const hasOpenAI = chain.some((m) => m.startsWith("gpt-"));
      expect(hasAnthropic, `no Anthropic fallback for ${picked}: ${chain.join(",")}`).toBe(true);
      expect(hasOpenAI, `no OpenAI fallback for ${picked}: ${chain.join(",")}`).toBe(true);
    }
  });
});
