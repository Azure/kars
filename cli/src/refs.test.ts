// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { buildInferencePolicy } from "./refs.js";

describe("buildInferencePolicy — requirePromptShields default", () => {
  const base = { sandboxName: "demo", namespace: "kars-system", model: "gpt-4.1" };

  it("defaults requirePromptShields to false (bare Foundry safe)", () => {
    const cr = buildInferencePolicy({ ...base });
    const spec = cr.spec as { contentSafety: { requirePromptShields: boolean } };
    expect(spec.contentSafety.requirePromptShields).toBe(false);
  });

  it("stays false when promptShields is explicitly false", () => {
    const cr = buildInferencePolicy({ ...base, promptShields: false });
    const spec = cr.spec as { contentSafety: { requirePromptShields: boolean } };
    expect(spec.contentSafety.requirePromptShields).toBe(false);
  });

  it("opts in only when promptShields is explicitly true", () => {
    const cr = buildInferencePolicy({ ...base, promptShields: true });
    const spec = cr.spec as { contentSafety: { requirePromptShields: boolean } };
    expect(spec.contentSafety.requirePromptShields).toBe(true);
  });
});
