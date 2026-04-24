import { describe, it, expect } from "vitest";
import { __test } from "./convert.js";

describe("convertCommand — target parsing", () => {
  it("accepts all three targets", () => {
    for (const t of __test.TARGETS) {
      expect(__test.parseTarget(t)).toBe(t);
    }
  });

  it("rejects unknown targets", () => {
    expect(__test.parseTarget("yaml")).toBeUndefined();
    expect(__test.parseTarget("native")).toBeUndefined();
    expect(__test.parseTarget("")).toBeUndefined();
  });

  it("rejects undefined target", () => {
    expect(__test.parseTarget(undefined)).toBeUndefined();
  });
});
