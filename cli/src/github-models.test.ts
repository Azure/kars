// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach } from "vitest";

import {
  isToolCapable, tierLabel, normalizeSecretValue,
  buildCuratedChoices, buildAllToolCapableChoices,
  validateModelAgainstCatalog, fetchCatalog, RECOMMENDED_MODELS,
  type CatalogModel,
} from "./github-models.js";

const mkModel = (over: Partial<CatalogModel> = {}): CatalogModel => ({
  id: "openai/gpt-4.1",
  publisher: "OpenAI",
  capabilities: ["tool-calling", "streaming"],
  rate_limit_tier: "low",
  limits: { max_input_tokens: 128_000 },
  ...over,
});

describe("normalizeSecretValue", () => {
  it("strips leading bot from telegram-token", () => {
    expect(normalizeSecretValue("telegram-token", "bot123:abc")).toBe("123:abc");
  });

  it("strips bot from telegram-token dot-suffix variants", () => {
    expect(normalizeSecretValue("telegram-token.cloud", "bot999:xyz")).toBe("999:xyz");
  });

  it("leaves non-telegram keys untouched", () => {
    expect(normalizeSecretValue("brave-api-key", "bot-test-value")).toBe("bot-test-value");
  });

  it("trims whitespace", () => {
    expect(normalizeSecretValue("brave-api-key", "  abc  ")).toBe("abc");
  });

  it("handles tokens that don't start with bot", () => {
    expect(normalizeSecretValue("telegram-token", "123:abc")).toBe("123:abc");
  });
});

describe("isToolCapable", () => {
  it("matches tool-calling capability", () => {
    expect(isToolCapable(mkModel({ capabilities: ["tool-calling"] }))).toBe(true);
  });

  it("matches legacy tools capability for forward-compat", () => {
    expect(isToolCapable(mkModel({ capabilities: ["tools"] }))).toBe(true);
  });

  it("returns false when only streaming is supported", () => {
    expect(isToolCapable(mkModel({ capabilities: ["streaming"] }))).toBe(false);
  });

  it("tolerates missing capabilities", () => {
    expect(isToolCapable(mkModel({ capabilities: undefined }))).toBe(false);
  });
});

describe("tierLabel", () => {
  it.each([
    ["low", "free"],
    ["high", "free"],
    ["custom", "paid"],
    ["embeddings", "embed"],
    [undefined, "unknown"],
    ["surprise", "unknown"],
  ])("maps %s → %s", (input, expected) => {
    expect(tierLabel(input as never)).toBe(expected);
  });
});

describe("validateModelAgainstCatalog", () => {
  const catalog: CatalogModel[] = [
    mkModel({ id: "openai/gpt-4.1" }),
    mkModel({ id: "openai/gpt-4.1-mini" }),
    mkModel({ id: "openai/gpt-5", capabilities: ["streaming"] }),
  ];

  it("accepts a tool-capable catalog model", () => {
    const r = validateModelAgainstCatalog("openai/gpt-4.1", catalog);
    expect(r.ok).toBe(true);
  });

  it("rejects models not in catalog with suggestion", () => {
    const r = validateModelAgainstCatalog("openai/gpt-4-1", catalog);
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.reason).toBe("not-found");
      expect(r.suggestion).toBe("openai/gpt-4.1");
    }
  });

  it("rejects models without tool-calling", () => {
    const r = validateModelAgainstCatalog("openai/gpt-5", catalog);
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toBe("no-tools");
  });

  it("does not suggest nonsense for tiny inputs", () => {
    const r = validateModelAgainstCatalog("xx", catalog);
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.reason).toBe("not-found");
      expect(r.suggestion).toBeUndefined();
    }
  });
});

describe("buildCuratedChoices", () => {
  it("only emits curated entries that exist in catalog", () => {
    const catalog: CatalogModel[] = [mkModel({ id: "openai/gpt-4.1" })];
    const choices = buildCuratedChoices(catalog);
    const values = choices.map(c => c.value).filter(v => !v.startsWith("__"));
    expect(values).toEqual(["openai/gpt-4.1"]);
  });

  it("filters out non-tool-capable curated entries", () => {
    const catalog: CatalogModel[] = [mkModel({ id: "openai/gpt-4.1", capabilities: [] })];
    const choices = buildCuratedChoices(catalog);
    const ids = choices.map(c => c.value).filter(v => !v.startsWith("__"));
    expect(ids).not.toContain("openai/gpt-4.1");
  });

  it("always appends show-all and custom escape hatches", () => {
    const choices = buildCuratedChoices([]);
    const values = choices.map(c => c.value);
    expect(values).toContain("__show_all__");
    expect(values).toContain("__custom__");
  });

  it("groups paid tier behind a divider", () => {
    const catalog: CatalogModel[] = RECOMMENDED_MODELS.map(r =>
      mkModel({ id: r.id, rate_limit_tier: r.tier === "paid" ? "custom" : "low" }),
    );
    const choices = buildCuratedChoices(catalog);
    const dividerIdx = choices.findIndex(c => c.value === "__divider_paid__");
    expect(dividerIdx).toBeGreaterThan(0);
    expect(choices[dividerIdx]!.isDivider).toBe(true);
  });
});

describe("buildAllToolCapableChoices", () => {
  it("groups by publisher and skips non-tool models", () => {
    const catalog: CatalogModel[] = [
      mkModel({ id: "openai/x", publisher: "OpenAI" }),
      mkModel({ id: "openai/y-no-tools", publisher: "OpenAI", capabilities: ["streaming"] }),
      mkModel({ id: "meta/z", publisher: "Meta" }),
    ];
    const choices = buildAllToolCapableChoices(catalog);
    const ids = choices.filter(c => !c.isDivider).map(c => c.value);
    expect(ids).toContain("openai/x");
    expect(ids).toContain("meta/z");
    expect(ids).not.toContain("openai/y-no-tools");
    const dividers = choices.filter(c => c.isDivider).map(c => c.label);
    expect(dividers.some(d => d.includes("OpenAI"))).toBe(true);
    expect(dividers.some(d => d.includes("Meta"))).toBe(true);
  });
});

describe("fetchCatalog", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns ok=true with parsed models on 200", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => [{ id: "openai/x", capabilities: ["tool-calling"] }],
    })));
    const r = await fetchCatalog("ghp_test");
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.models).toHaveLength(1);
  });

  it("returns ok=false on HTTP error", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({
      ok: false,
      status: 401,
      text: async () => "unauthorized",
    })));
    const r = await fetchCatalog("invalid");
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.status).toBe(401);
      expect(r.message).toContain("unauthorized");
    }
  });

  it("returns ok=false when response is not an array", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => ({
      ok: true,
      status: 200,
      json: async () => ({ unexpected: "shape" }),
    })));
    const r = await fetchCatalog("ghp_test");
    expect(r.ok).toBe(false);
  });

  it("returns ok=false on network error", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new Error("ENOTFOUND"); }));
    const r = await fetchCatalog("ghp_test");
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.status).toBe(0);
      expect(r.message).toContain("ENOTFOUND");
    }
  });
});
