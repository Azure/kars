import { describe, expect, it } from "vitest";

import { latin1Safe } from "./artifact-collect.js";

describe("latin1Safe", () => {
  it("transliterates semantic punctuation and drops decorative symbols", () => {
    expect(latin1Safe("🔒 non‑root → ready ✓")).toBe(" non-root -> ready ");
  });

  it("never introduces replacement question marks", () => {
    expect(latin1Safe("1️⃣ Role Plan — “safe”")).not.toContain("?");
  });
});
