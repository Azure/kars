// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi } from "vitest";
import {
  buildPath,
  renderList,
  renderTop,
  unixToIso,
  type BlockedResponse,
  type BlockedTopResponse,
} from "./blocked.js";

describe("egress blocked CLI — buildPath", () => {
  it("returns the bare list path with no options", () => {
    expect(buildPath({})).toBe("/internal/egress/blocked");
  });

  it("encodes --since", () => {
    expect(buildPath({ since: "-10m" })).toBe(
      "/internal/egress/blocked?since=-10m"
    );
  });

  it("encodes --since with RFC 3339 (URL-safe)", () => {
    const got = buildPath({ since: "2024-01-15T10:30:45Z" });
    // URLSearchParams percent-encodes the colons; both forms acceptable.
    expect(got).toContain("/internal/egress/blocked?since=");
    expect(decodeURIComponent(got.split("=")[1])).toBe("2024-01-15T10:30:45Z");
  });

  it("returns the bare top path with no options", () => {
    expect(buildPath({ top: true })).toBe("/internal/egress/blocked/top");
  });

  it("encodes --top --window --n", () => {
    expect(buildPath({ top: true, window: "1h", n: "20" })).toBe(
      "/internal/egress/blocked/top?window=1h&n=20"
    );
  });

  it("--top overrides --since (since is list-only)", () => {
    // since is documented as list-only; top uses window. Confirm builder
    // does NOT leak `since` into the top path.
    expect(buildPath({ top: true, since: "-10m" })).toBe(
      "/internal/egress/blocked/top"
    );
  });
});

describe("egress blocked CLI — unixToIso", () => {
  it("0 → 'epoch'", () => {
    expect(unixToIso(0)).toBe("epoch");
  });

  it("known timestamp formats with .000Z suffix", () => {
    // 2024-01-15T10:30:45Z
    expect(unixToIso(1_705_314_645)).toBe("2024-01-15T10:30:45.000Z");
  });
});

describe("egress blocked CLI — renderers", () => {
  it("renderList prints 'No blocked attempts' on empty response", () => {
    const spy = vi.spyOn(console, "log").mockImplementation(() => {});
    const resp: BlockedResponse = {
      schema_version: 1,
      total: 0,
      count: 0,
      since_unix: 0,
      entries: [],
    };
    renderList("sb1", "kars-sb1", resp);
    const out = spy.mock.calls.map((c) => String(c[0])).join("\n");
    expect(out).toContain("No blocked attempts");
    spy.mockRestore();
  });

  it("renderList prints HOST/PORT/SANDBOX/LAST_SEEN/COUNT header", () => {
    const spy = vi.spyOn(console, "log").mockImplementation(() => {});
    const resp: BlockedResponse = {
      schema_version: 1,
      total: 1,
      count: 1,
      since_unix: 0,
      entries: [
        {
          host: "evil.example.com",
          port: 443,
          source_sandbox: "sb1",
          count: 3,
          first_seen_unix: 1_705_314_645,
          last_seen_unix: 1_705_314_700,
          first_seen: "2024-01-15T10:30:45.000Z",
          last_seen: "2024-01-15T10:31:40.000Z",
        },
      ],
    };
    renderList("sb1", "kars-sb1", resp);
    const out = spy.mock.calls.map((c) => String(c[0])).join("\n");
    expect(out).toContain("HOST");
    expect(out).toContain("PORT");
    expect(out).toContain("SANDBOX");
    expect(out).toContain("LAST_SEEN");
    expect(out).toContain("COUNT");
    expect(out).toContain("evil.example.com");
    expect(out).toContain("443");
    expect(out).toContain("sb1");
    expect(out).toContain("2024-01-15T10:31:40.000Z");
    expect(out).toContain(" 3"); // count column
    spy.mockRestore();
  });

  it("renderList includes since-filter line when since_unix > 0", () => {
    const spy = vi.spyOn(console, "log").mockImplementation(() => {});
    const resp: BlockedResponse = {
      schema_version: 1,
      total: 0,
      count: 0,
      since_unix: 1_705_314_645,
      entries: [],
    };
    renderList("sb1", "kars-sb1", resp);
    const out = spy.mock.calls.map((c) => String(c[0])).join("\n");
    expect(out).toContain("Filter: since 2024-01-15T10:30:45.000Z");
    spy.mockRestore();
  });

  it("renderTop prints HOST/COUNT header with window line", () => {
    const spy = vi.spyOn(console, "log").mockImplementation(() => {});
    const resp: BlockedTopResponse = {
      schema_version: 1,
      since_unix: 1_705_314_645,
      window: "5m",
      n: 10,
      top: [
        { host: "a.example.com", count: 5 },
        { host: "b.example.com", count: 2 },
      ],
    };
    renderTop("sb1", "kars-sb1", resp);
    const out = spy.mock.calls.map((c) => String(c[0])).join("\n");
    expect(out).toContain("Window: 5m");
    expect(out).toContain("top 10");
    expect(out).toContain("HOST");
    expect(out).toContain("COUNT");
    expect(out).toContain("a.example.com");
    expect(out).toContain(" 5");
    expect(out).toContain("b.example.com");
    spy.mockRestore();
  });

  it("renderTop prints 'No blocked attempts' on empty top", () => {
    const spy = vi.spyOn(console, "log").mockImplementation(() => {});
    const resp: BlockedTopResponse = {
      schema_version: 1,
      since_unix: 0,
      window: "5m",
      n: 10,
      top: [],
    };
    renderTop("sb1", "kars-sb1", resp);
    const out = spy.mock.calls.map((c) => String(c[0])).join("\n");
    expect(out).toContain("No blocked attempts");
    spy.mockRestore();
  });
});
