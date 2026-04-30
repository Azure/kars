// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Tests for `runtime.ts` — the CLI's runtime-aware helpers (S10.A5).
 */
import { describe, expect, it } from "vitest";
import {
  agentContainerName,
  assertRuntimeWired,
  buildRuntimeBlock,
  flagToKind,
  runtimeKindFromCr,
} from "./runtime.js";

describe("flagToKind", () => {
  it("maps each kebab-case flag to its PascalCase RuntimeKind", () => {
    expect(flagToKind("openclaw")).toBe("OpenClaw");
    expect(flagToKind("openai-agents")).toBe("OpenAIAgents");
    expect(flagToKind("microsoft-agent-framework")).toBe("MicrosoftAgentFramework");
    expect(flagToKind("byo")).toBe("BYO");
  });

  it("is case-insensitive", () => {
    expect(flagToKind("OpenClaw")).toBe("OpenClaw");
    expect(flagToKind("OPENAI-AGENTS")).toBe("OpenAIAgents");
  });

  it("throws with a helpful error on unknown flags", () => {
    expect(() => flagToKind("autogen")).toThrow(/Unknown --runtime value: autogen/);
    expect(() => flagToKind("")).toThrow(/Unknown --runtime value/);
  });
});

describe("assertRuntimeWired", () => {
  it("accepts the four wired runtimes", () => {
    expect(() => assertRuntimeWired("OpenClaw")).not.toThrow();
    expect(() => assertRuntimeWired("OpenAIAgents")).not.toThrow();
    expect(() => assertRuntimeWired("MicrosoftAgentFramework")).not.toThrow();
    expect(() => assertRuntimeWired("BYO")).not.toThrow();
  });

  it("rejects the three Tier-2 placeholders", () => {
    expect(() => assertRuntimeWired("SemanticKernel")).toThrow(/no adapter wired/);
    expect(() => assertRuntimeWired("LangGraph")).toThrow(/no adapter wired/);
    expect(() => assertRuntimeWired("Anthropic")).toThrow(/no adapter wired/);
  });

  it("the rejection message names the wired runtimes for discoverability", () => {
    try {
      assertRuntimeWired("LangGraph");
      throw new Error("must have thrown");
    } catch (e) {
      const msg = (e as Error).message;
      expect(msg).toContain("OpenClaw");
      expect(msg).toContain("OpenAIAgents");
      expect(msg).toContain("MicrosoftAgentFramework");
      expect(msg).toContain("BYO");
    }
  });
});

describe("agentContainerName", () => {
  it("returns 'openclaw' for the OpenClaw runtime (legacy container name)", () => {
    expect(agentContainerName("OpenClaw")).toBe("openclaw");
  });

  it("returns 'agent' for every non-OpenClaw runtime", () => {
    expect(agentContainerName("OpenAIAgents")).toBe("agent");
    expect(agentContainerName("MicrosoftAgentFramework")).toBe("agent");
    expect(agentContainerName("BYO")).toBe("agent");
    expect(agentContainerName("SemanticKernel")).toBe("agent");
    expect(agentContainerName("LangGraph")).toBe("agent");
    expect(agentContainerName("Anthropic")).toBe("agent");
  });
});

describe("runtimeKindFromCr", () => {
  it("reads spec.runtime.kind when present", () => {
    expect(runtimeKindFromCr({ spec: { runtime: { kind: "OpenAIAgents" } } })).toBe("OpenAIAgents");
    expect(runtimeKindFromCr({ spec: { runtime: { kind: "MicrosoftAgentFramework" } } }))
      .toBe("MicrosoftAgentFramework");
    expect(runtimeKindFromCr({ spec: { runtime: { kind: "BYO" } } })).toBe("BYO");
  });

  it("falls back to OpenClaw for legacy / missing runtime block", () => {
    expect(runtimeKindFromCr({})).toBe("OpenClaw");
    expect(runtimeKindFromCr({ spec: {} })).toBe("OpenClaw");
    expect(runtimeKindFromCr({ spec: { runtime: {} } })).toBe("OpenClaw");
    expect(runtimeKindFromCr(null)).toBe("OpenClaw");
    expect(runtimeKindFromCr(undefined)).toBe("OpenClaw");
  });

  it("falls back to OpenClaw when the kind value is unknown (defensive)", () => {
    // A future controller version may add a new kind; the CLI must
    // not crash — fall back to the safe default.
    expect(runtimeKindFromCr({ spec: { runtime: { kind: "FutureKind" } } })).toBe("OpenClaw");
  });
});

describe("buildRuntimeBlock", () => {
  it("emits the OpenClaw block with expected defaults", () => {
    const block = buildRuntimeBlock({ kind: "OpenClaw", model: "gpt-4.1" }) as Record<string, unknown>;
    expect(block.kind).toBe("OpenClaw");
    const oc = block.openclaw as Record<string, unknown>;
    expect(oc.version).toBe("2026.3.13");
    expect((oc.config as Record<string, Record<string, string>>).agent.model).toBe("azure/gpt-4.1");
    // No image override.
    expect("image" in oc).toBe(false);
  });

  it("respects --image override for OpenClaw", () => {
    const block = buildRuntimeBlock({
      kind: "OpenClaw",
      model: "gpt-4.1",
      image: "myacr.azurecr.io/openclaw:custom",
    }) as Record<string, unknown>;
    expect((block.openclaw as Record<string, unknown>).image).toBe("myacr.azurecr.io/openclaw:custom");
  });

  it("emits the OpenAIAgents block with no required user input", () => {
    const block = buildRuntimeBlock({ kind: "OpenAIAgents" }) as Record<string, unknown>;
    expect(block.kind).toBe("OpenAIAgents");
    expect(block.openaiAgents).toEqual({});
  });

  it("emits the MicrosoftAgentFramework block defaulting to python", () => {
    const block = buildRuntimeBlock({ kind: "MicrosoftAgentFramework" }) as Record<string, unknown>;
    expect(block.kind).toBe("MicrosoftAgentFramework");
    expect((block.microsoftAgentFramework as Record<string, string>).language).toBe("python");
  });

  it("rejects --maf-language dotnet client-side (Phase 3 / upstream-blocked)", () => {
    expect(() => buildRuntimeBlock({
      kind: "MicrosoftAgentFramework",
      mafLanguage: "dotnet",
    })).toThrow(/dotnet.*not yet wired.*Phase 3/);
  });

  it("emits the BYO block with the supplied image and contract version", () => {
    const block = buildRuntimeBlock({
      kind: "BYO",
      byoImage: "ghcr.io/team/agent:1.0",
    }) as Record<string, unknown>;
    expect(block.kind).toBe("BYO");
    const byo = block.byo as Record<string, string>;
    expect(byo.image).toBe("ghcr.io/team/agent:1.0");
    expect(byo.contractVersion).toBe("v1");
  });

  it("requires --byo-image when --runtime byo", () => {
    expect(() => buildRuntimeBlock({ kind: "BYO" })).toThrow(/--byo-image is required/);
  });

  it("respects custom BYO contract version", () => {
    const block = buildRuntimeBlock({
      kind: "BYO",
      byoImage: "ghcr.io/team/agent:1.0",
      byoContractVersion: "v2",
    }) as Record<string, unknown>;
    expect((block.byo as Record<string, string>).contractVersion).toBe("v2");
  });
});
