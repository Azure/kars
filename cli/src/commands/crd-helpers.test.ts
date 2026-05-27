// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import {
  formatAge,
  formatTable,
  parseKVPairs,
  parseSpecFile,
  stripUndefined,
  validateName,
  buildCR,
  toYaml,
} from "./crd-helpers.js";

describe("crd-helpers — validateName", () => {
  it("accepts valid DNS-1123 labels", () => {
    expect(validateName("foo")).toEqual([]);
    expect(validateName("foo-bar-baz")).toEqual([]);
    expect(validateName("a1")).toEqual([]);
    expect(validateName("a")).toEqual([]);
  });

  it("rejects empty / missing names", () => {
    expect(validateName("").length).toBeGreaterThan(0);
  });

  it("rejects uppercase, leading/trailing hyphens, dots, underscores", () => {
    expect(validateName("Foo")[0]).toContain("DNS-1123");
    expect(validateName("-foo")[0]).toContain("DNS-1123");
    expect(validateName("foo-")[0]).toContain("DNS-1123");
    expect(validateName("foo.bar")[0]).toContain("DNS-1123");
    expect(validateName("foo_bar")[0]).toContain("DNS-1123");
  });

  it("rejects names longer than 63 characters", () => {
    const long = "a".repeat(64);
    const errs = validateName(long);
    expect(errs.some((e) => e.includes("63"))).toBe(true);
  });
});

describe("crd-helpers — parseKVPairs", () => {
  it("parses simple key=value pairs", () => {
    expect(parseKVPairs(["a=1", "b=two"])).toEqual({ a: "1", b: "two" });
  });

  it("returns empty object for undefined / []", () => {
    expect(parseKVPairs(undefined)).toEqual({});
    expect(parseKVPairs([])).toEqual({});
  });

  it("throws on malformed entries", () => {
    expect(() => parseKVPairs(["nope"])).toThrow();
    expect(() => parseKVPairs(["=value"])).toThrow();
    expect(() => parseKVPairs(["key="])).toThrow();
  });

  it("trims whitespace around keys/values", () => {
    expect(parseKVPairs([" k = v "])).toEqual({ k: "v" });
  });
});

describe("crd-helpers — stripUndefined", () => {
  it("removes undefined keys recursively", () => {
    expect(stripUndefined({ a: 1, b: undefined, c: { d: undefined, e: 2 } })).toEqual({
      a: 1,
      c: { e: 2 },
    });
  });

  it("filters undefined entries from arrays", () => {
    const arr = [1, undefined, 2];
    expect(stripUndefined(arr)).toEqual([1, 2]);
  });

  it("preserves null (RFC 7396 explicit unset)", () => {
    expect(stripUndefined({ a: null })).toEqual({ a: null });
  });
});

describe("crd-helpers — parseSpecFile", () => {
  it("accepts JSON object content", () => {
    expect(parseSpecFile('{"foo": "bar"}')).toEqual({ foo: "bar" });
  });

  it("accepts YAML content", () => {
    expect(parseSpecFile("foo: bar\nbaz: 1")).toEqual({ foo: "bar", baz: 1 });
  });

  it("unwraps a top-level spec: block", () => {
    expect(parseSpecFile("spec:\n  foo: bar")).toEqual({ foo: "bar" });
  });

  it("rejects empty content / arrays / scalars", () => {
    expect(() => parseSpecFile("")).toThrow();
    expect(() => parseSpecFile("[1, 2]")).toThrow();
    expect(() => parseSpecFile("- a\n- b")).toThrow();
  });
});

describe("crd-helpers — buildCR / toYaml", () => {
  it("produces apiVersion + kind + metadata + spec", () => {
    const cr = buildCR("ToolPolicy", "x", "ns1", { appliesTo: { tool: "*" } });
    expect(cr.apiVersion).toBe("kars.azure.com/v1alpha1");
    expect(cr.kind).toBe("ToolPolicy");
    expect(cr.metadata).toEqual({ name: "x", namespace: "ns1" });
    expect(cr.spec).toEqual({ appliesTo: { tool: "*" } });
  });

  it("renders to YAML deterministically", () => {
    const yaml = toYaml({ a: 1, b: { c: 2 } });
    expect(yaml).toContain("a: 1");
    expect(yaml).toContain("b:");
    expect(yaml).toContain("c: 2");
  });
});

describe("crd-helpers — formatAge", () => {
  const NOW = new Date("2025-01-01T00:00:00Z");

  it("renders seconds / minutes / hours / days", () => {
    expect(formatAge("2024-12-31T23:59:30Z", NOW)).toBe("30s");
    expect(formatAge("2024-12-31T23:55:00Z", NOW)).toBe("5m");
    expect(formatAge("2024-12-31T20:00:00Z", NOW)).toBe("4h");
    expect(formatAge("2024-12-25T00:00:00Z", NOW)).toBe("7d");
  });

  it("handles unknown / malformed timestamps", () => {
    expect(formatAge(undefined, NOW)).toBe("<unknown>");
    expect(formatAge("not-a-date", NOW)).toBe("<unknown>");
  });
});

describe("crd-helpers — formatTable", () => {
  it("aligns headers and rows", () => {
    const out = formatTable(
      ["A", "BB"],
      [{ cells: ["xxxx", "y"] }, { cells: ["1", "22"] }],
    );
    const lines = out.split("\n");
    expect(lines[0]).toMatch(/^A/);
    expect(lines[1].startsWith("xxxx")).toBe(true);
    expect(lines.length).toBe(3);
  });
});
