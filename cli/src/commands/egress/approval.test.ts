// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import {
  __test,
  allowExtraCommand,
  approvalsCommand,
  revokeCommand,
} from "./approval.js";

const {
  parseHostEndpoint,
  parseIsoDurationSecs,
  validateReasonText,
  buildEgressApprovalSpecFromFlags,
  deriveApprovalName,
  summarizeApprovalRow,
  HARD_TTL_CEILING_SECONDS,
  REASON_MAX_BYTES,
  HOSTS_MAX,
} = __test;

describe("parseHostEndpoint", () => {
  it("parses a bare hostname with no port", () => {
    expect(parseHostEndpoint("example.com")).toEqual({ host: "example.com" });
  });

  it("parses host:port", () => {
    expect(parseHostEndpoint("api.example.com:443")).toEqual({
      host: "api.example.com",
      port: 443,
    });
  });

  it("rejects URLs with schemes", () => {
    expect(() => parseHostEndpoint("https://example.com")).toThrow(/bare hostname/);
  });

  it("rejects paths", () => {
    expect(() => parseHostEndpoint("example.com/path")).toThrow(/bare hostname/);
  });

  it("rejects empty", () => {
    expect(() => parseHostEndpoint("")).toThrow(/empty/);
    expect(() => parseHostEndpoint("   ")).toThrow(/empty/);
  });

  it("rejects port 0 and port > 65535", () => {
    expect(() => parseHostEndpoint("x.com:0")).toThrow(/invalid port/);
    expect(() => parseHostEndpoint("x.com:70000")).toThrow(/invalid port/);
  });

  it("rejects non-numeric port", () => {
    expect(() => parseHostEndpoint("x.com:http")).toThrow(/invalid port/);
  });

  it("rejects missing host before ':'", () => {
    expect(() => parseHostEndpoint(":443")).toThrow(/missing the hostname/);
  });
});

describe("parseIsoDurationSecs", () => {
  it.each([
    ["PT15M", 900],
    ["PT4H", 14400],
    ["PT1H30M", 5400],
    ["P1D", 86400],
    ["PT30S", 30],
    ["P1DT2H3M4S", 86400 + 2 * 3600 + 3 * 60 + 4],
  ])("parses %s = %d seconds", (input, expected) => {
    expect(parseIsoDurationSecs(input)).toBe(expected);
  });

  it.each([
    "",
    "1H",
    "P",
    "PT",
    "P1W", // weeks unsupported
    "P1Y", // years unsupported
    "P1M", // months in date part — unsupported (we only accept D before T)
    "P1H", // H before T
    "PT1D", // D after T
    "PT0S", // zero
    "P0D", // zero
    "PT-1H", // negative not allowed; minus is not a digit
  ])("rejects %s", (input) => {
    expect(() => parseIsoDurationSecs(input)).toThrow();
  });
});

describe("validateReasonText", () => {
  it("accepts normal text", () => {
    expect(validateReasonText("urgent debugging session for INC-12345")).toBeNull();
  });

  it("accepts tab / LF / CR", () => {
    expect(validateReasonText("multi\nline\twith\rcr")).toBeNull();
  });

  it("rejects empty", () => {
    expect(validateReasonText("")).toMatch(/non-empty/);
  });

  it("rejects oversize", () => {
    const big = "a".repeat(REASON_MAX_BYTES + 1);
    expect(validateReasonText(big)).toMatch(/≤ 512/);
  });

  it("rejects ASCII control byte (NUL)", () => {
    expect(validateReasonText("hello\x00world")).toMatch(/control byte 0x00/);
  });

  it("rejects ASCII control byte (DEL)", () => {
    expect(validateReasonText("x\x7Fy")).toMatch(/control byte 0x7f/);
  });
});

describe("buildEgressApprovalSpecFromFlags", () => {
  it("builds a minimal valid spec", () => {
    const spec = buildEgressApprovalSpecFromFlags("my-agent", {
      namespace: "default",
      host: ["api.example.com"],
      ttl: "PT15M",
      reason: "debugging customer issue",
    });
    expect(spec).toEqual({
      sandbox: "my-agent",
      hosts: [{ host: "api.example.com" }],
      reason: "debugging customer issue",
      ttl: "PT15M",
    });
  });

  it("includes port when provided", () => {
    const spec = buildEgressApprovalSpecFromFlags("a", {
      namespace: "default",
      host: ["x.com:8443"],
      ttl: "PT1H",
      reason: "test",
    });
    expect((spec.hosts as { port: number }[])[0]?.port).toBe(8443);
  });

  it("includes ticket when provided", () => {
    const spec = buildEgressApprovalSpecFromFlags("a", {
      namespace: "default",
      host: ["x.com"],
      ttl: "PT1H",
      reason: "test",
      ticket: "INC-12345",
    });
    expect(spec.ticket).toBe("INC-12345");
  });

  it("requires at least one --host", () => {
    expect(() =>
      buildEgressApprovalSpecFromFlags("a", {
        namespace: "default",
        host: [],
        ttl: "PT1H",
        reason: "x",
      }),
    ).toThrow(/at least one --host/);
  });

  it(`rejects more than ${HOSTS_MAX} hosts`, () => {
    const tooMany = Array.from({ length: HOSTS_MAX + 1 }, (_, i) => `h${i}.com`);
    expect(() =>
      buildEgressApprovalSpecFromFlags("a", {
        namespace: "default",
        host: tooMany,
        ttl: "PT1H",
        reason: "x",
      }),
    ).toThrow(/too many --host/);
  });

  it("requires --ttl", () => {
    expect(() =>
      buildEgressApprovalSpecFromFlags("a", {
        namespace: "default",
        host: ["x.com"],
        reason: "x",
      }),
    ).toThrow(/--ttl is required/);
  });

  it("requires --reason", () => {
    expect(() =>
      buildEgressApprovalSpecFromFlags("a", {
        namespace: "default",
        host: ["x.com"],
        ttl: "PT1H",
      }),
    ).toThrow(/--reason is required/);
  });

  it("rejects --ttl over 7d hard ceiling", () => {
    expect(() =>
      buildEgressApprovalSpecFromFlags("a", {
        namespace: "default",
        host: ["x.com"],
        ttl: "P8D",
        reason: "x",
      }),
    ).toThrow(/hard ceiling of 7 days/);
  });

  it("accepts --ttl at exactly the hard ceiling", () => {
    const spec = buildEgressApprovalSpecFromFlags("a", {
      namespace: "default",
      host: ["x.com"],
      ttl: "P7D",
      reason: "x",
    });
    expect(spec.ttl).toBe("P7D");
    expect(HARD_TTL_CEILING_SECONDS).toBe(7 * 24 * 3600);
  });

  it("rejects empty sandbox name", () => {
    expect(() =>
      buildEgressApprovalSpecFromFlags("", {
        namespace: "default",
        host: ["x.com"],
        ttl: "PT1H",
        reason: "x",
      }),
    ).toThrow(/sandbox name is required/);
  });

  it("rejects oversize --ticket", () => {
    expect(() =>
      buildEgressApprovalSpecFromFlags("a", {
        namespace: "default",
        host: ["x.com"],
        ttl: "PT1H",
        reason: "x",
        ticket: "a".repeat(129),
      }),
    ).toThrow(/≤ 128 bytes/);
  });

  it("rejects --reason with control byte", () => {
    expect(() =>
      buildEgressApprovalSpecFromFlags("a", {
        namespace: "default",
        host: ["x.com"],
        ttl: "PT1H",
        reason: "with\x00null",
      }),
    ).toThrow(/control byte/);
  });
});

describe("deriveApprovalName", () => {
  it("produces a deterministic name from sandbox + timestamp", () => {
    const now = new Date("2026-05-14T12:30:45.123Z");
    expect(deriveApprovalName("my-agent", now)).toBe(
      "my-agent-extra-20260514-123045z",
    );
  });

  it("uses lowercase, dashes only — valid DNS-1123 label", () => {
    const now = new Date("2026-05-14T00:00:00Z");
    const n = deriveApprovalName("agent1", now);
    expect(n).toMatch(/^[a-z0-9]([-a-z0-9]*[a-z0-9])?$/);
  });
});

describe("summarizeApprovalRow", () => {
  const NOW = new Date("2026-05-14T12:00:00Z");

  it("shows 'in Xm' when expires in future", () => {
    const row = summarizeApprovalRow(
      {
        metadata: { name: "ap1", creationTimestamp: "2026-05-14T11:45:00Z" },
        spec: {
          sandbox: "a",
          hosts: [{ host: "x.com" }, { host: "y.com" }],
          ttl: "PT15M",
        },
        status: { phase: "Active", expiresAt: "2026-05-14T12:15:00Z" },
      },
      NOW,
    );
    expect(row).toEqual(["ap1", "2", "PT15M", "in 15m", "Active", "15m"]);
  });

  it("shows 'expired' when expiresAt is in the past", () => {
    const row = summarizeApprovalRow(
      {
        metadata: { name: "ap2", creationTimestamp: "2026-05-14T11:00:00Z" },
        spec: { sandbox: "a", hosts: [{ host: "x.com" }], ttl: "PT15M" },
        status: { phase: "Expired", expiresAt: "2026-05-14T11:15:00Z" },
      },
      NOW,
    );
    expect(row[3]).toBe("expired");
    expect(row[4]).toBe("Expired");
  });

  it("falls back to '-' when status/spec are missing", () => {
    const row = summarizeApprovalRow(
      { metadata: { name: "ap3" }, spec: {} },
      NOW,
    );
    expect(row).toEqual(["ap3", "0", "-", "-", "-", "<unknown>"]);
  });
});

describe("commander wiring", () => {
  it("allow-extra exposes the right flags", () => {
    const cmd = allowExtraCommand();
    expect(cmd.name()).toBe("allow-extra");
    const flags = cmd.options.map((o) => o.long);
    expect(flags).toEqual(
      expect.arrayContaining([
        "--namespace",
        "--host",
        "--ttl",
        "--reason",
        "--ticket",
        "--name",
        "--from-file",
      ]),
    );
  });

  it("approvals takes a sandbox arg + --namespace", () => {
    const cmd = approvalsCommand();
    expect(cmd.name()).toBe("approvals");
    expect(cmd.options.map((o) => o.long)).toContain("--namespace");
  });

  it("revoke takes a name arg + --namespace + --no-prompt", () => {
    const cmd = revokeCommand();
    expect(cmd.name()).toBe("revoke");
    const flags = cmd.options.map((o) => o.long);
    expect(flags).toEqual(expect.arrayContaining(["--namespace", "--no-prompt"]));
  });
});
