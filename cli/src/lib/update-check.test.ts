// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import os from "node:os";
import path from "node:path";
import { promises as fs } from "node:fs";
import {
  updateCheckDisabled,
  summarizeReleaseBody,
  fetchLatestVersion,
  fetchChangelogSummary,
  checkForCliUpdate,
  CLI_PACKAGE,
} from "./update-check.js";

// Redirect the on-disk cache into a throwaway temp dir so tests never touch the
// developer's real ~/.kars/update-check.json.
let stateDir: string;
beforeEach(async () => {
  stateDir = await fs.mkdtemp(path.join(os.tmpdir(), "kars-update-test-"));
  process.env.KARS_STATE_DIR = stateDir;
});
afterEach(async () => {
  delete process.env.KARS_STATE_DIR;
  await fs.rm(stateDir, { recursive: true, force: true });
});

describe("updateCheckDisabled", () => {
  it("is enabled by default", () => {
    expect(updateCheckDisabled({})).toBe(false);
  });

  it("respects explicit opt-out env vars", () => {
    expect(updateCheckDisabled({ KARS_NO_UPDATE_CHECK: "1" })).toBe(true);
    expect(updateCheckDisabled({ KARS_NO_UPDATE_CHECK: "true" })).toBe(true);
    expect(updateCheckDisabled({ KARS_UPDATE_CHECK: "0" })).toBe(true);
    expect(updateCheckDisabled({ KARS_UPDATE_CHECK: "false" })).toBe(true);
  });

  it("disables in CI", () => {
    expect(updateCheckDisabled({ CI: "true" })).toBe(true);
    expect(updateCheckDisabled({ CI: "1" })).toBe(true);
    // A falsey CI value does not disable.
    expect(updateCheckDisabled({ CI: "0" })).toBe(false);
    expect(updateCheckDisabled({ CI: "false" })).toBe(false);
  });
});

describe("summarizeReleaseBody", () => {
  it("prefers the summary half of a 'vX — summary' release title", () => {
    expect(summarizeReleaseBody("v0.1.24 — MCP keepalive + egress auto-derive", ""))
      .toBe("MCP keepalive + egress auto-derive");
  });

  it("falls back to the first meaningful body line, stripped of markdown", () => {
    const body = "## What's changed\n\n- Fixed the Playwright `about:blank` session reaper\n";
    expect(summarizeReleaseBody(undefined, body))
      .toBe("Fixed the Playwright about:blank session reaper");
  });

  it("truncates very long summaries", () => {
    const long = "x".repeat(200);
    const out = summarizeReleaseBody(long, "")!;
    expect(out.length).toBeLessThanOrEqual(100);
    expect(out.endsWith("…")).toBe(true);
  });

  it("returns undefined when nothing usable is present", () => {
    expect(summarizeReleaseBody(undefined, undefined)).toBeUndefined();
    expect(summarizeReleaseBody("", "   \n  \n")).toBeUndefined();
  });
});

describe("fetchLatestVersion", () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("returns the version from the registry latest dist-tag", async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ version: "0.2.0" }), { status: 200 }),
    ) as unknown as typeof fetch;
    expect(await fetchLatestVersion(CLI_PACKAGE, 500)).toBe("0.2.0");
  });

  it("returns null on a non-200 response", async () => {
    globalThis.fetch = vi.fn(async () => new Response("nope", { status: 404 })) as unknown as typeof fetch;
    expect(await fetchLatestVersion(CLI_PACKAGE, 500)).toBeNull();
  });

  it("returns null (never throws) on a network error", async () => {
    globalThis.fetch = vi.fn(async () => {
      throw new Error("ECONNREFUSED");
    }) as unknown as typeof fetch;
    expect(await fetchLatestVersion(CLI_PACKAGE, 500)).toBeNull();
  });
});

describe("fetchChangelogSummary", () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("summarizes the GitHub release for the tag", async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ name: "v0.2.0 — big fixes", body: "" }), { status: 200 }),
    ) as unknown as typeof fetch;
    expect(await fetchChangelogSummary("0.2.0", 500)).toBe("big fixes");
  });

  it("returns undefined when the release is missing", async () => {
    globalThis.fetch = vi.fn(async () => new Response("", { status: 404 })) as unknown as typeof fetch;
    expect(await fetchChangelogSummary("9.9.9", 500)).toBeUndefined();
  });
});

describe("checkForCliUpdate", () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("returns null when disabled", async () => {
    const info = await checkForCliUpdate({ env: { KARS_NO_UPDATE_CHECK: "1" } });
    expect(info).toBeNull();
  });

  it("returns null when the registry reports an older/equal version", async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ version: "0.0.0" }), { status: 200 }),
    ) as unknown as typeof fetch;
    const info = await checkForCliUpdate({ force: true });
    expect(info).toBeNull();
  });

  it("reports an update when the registry has a newer version", async () => {
    globalThis.fetch = vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.includes("registry.npmjs.org")) {
        return new Response(JSON.stringify({ version: "999.0.0" }), { status: 200 });
      }
      // changelog
      return new Response(JSON.stringify({ name: "v999.0.0 — the future", body: "" }), { status: 200 });
    }) as unknown as typeof fetch;
    const info = await checkForCliUpdate({ force: true, withChangelog: true });
    expect(info).not.toBeNull();
    expect(info!.latest).toBe("999.0.0");
    expect(info!.changelog).toBe("the future");
  });
});
