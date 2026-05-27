// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { __test__ } from "./inspect.js";

const { renderEntry, shortDigest, formatLoadedAt, POLICY_KIND_LABEL } =
  __test__;

describe("inspect — shortDigest", () => {
  it("truncates sha256 digests to the controller convention", () => {
    // The controller logs digests at 12 hex chars; match that so an
    // operator can grep across both.
    expect(shortDigest("sha256:0123456789abcdef0123456789abcdef")).toBe(
      "sha256:0123456789ab…"
    );
  });

  it("returns the input unchanged when no algorithm prefix is present", () => {
    expect(shortDigest("abc")).toBe("abc");
  });

  it("preserves the algorithm prefix on alternate algos", () => {
    expect(shortDigest("sha512:abcdef0123456789aaaaaaaaaaaaaaaa")).toBe(
      "sha512:abcdef012345…"
    );
  });
});

describe("inspect — formatLoadedAt", () => {
  it("renders seconds-old timestamps as Ns", () => {
    const ts = new Date(Date.now() - 12_000).toISOString();
    expect(formatLoadedAt(ts)).toBe("12s ago");
  });

  it("renders minute-old timestamps as Nm", () => {
    const ts = new Date(Date.now() - 5 * 60_000).toISOString();
    expect(formatLoadedAt(ts)).toBe("5m ago");
  });

  it("renders hour-old timestamps as Nh", () => {
    const ts = new Date(Date.now() - 3 * 3_600_000).toISOString();
    expect(formatLoadedAt(ts)).toBe("3h ago");
  });

  it("renders day-old timestamps as Nd past the 48h threshold", () => {
    const ts = new Date(Date.now() - 5 * 86_400_000).toISOString();
    expect(formatLoadedAt(ts)).toBe("5d ago");
  });

  it("returns undefined on the loader's zero-marker (1970-01-01)", () => {
    // The router renders `SystemTime::UNIX_EPOCH` as "1970-01-01T00:00:00Z"
    // when an entry has never been loaded. The TS side returns
    // undefined so `renderEntry` substitutes the "(never loaded)"
    // label rather than printing "55y ago".
    expect(formatLoadedAt("1970-01-01T00:00:00Z")).toBeUndefined();
  });

  it("returns undefined on garbage input rather than NaN", () => {
    expect(formatLoadedAt("not a date")).toBeUndefined();
  });

  it("handles clock-skew (future) gracefully", () => {
    const ts = new Date(Date.now() + 60_000).toISOString();
    expect(formatLoadedAt(ts)).toBe("in the future");
  });
});

describe("inspect — renderEntry", () => {
  it("renders a healthy entry with source basename + short digest + age", () => {
    const out = renderEntry({
      kind: "ToolPolicy",
      digest: "sha256:0123456789abcdef0123456789abcdef",
      source_path: "/var/run/kars/toolpolicy/tools.json",
      loaded_at: new Date(Date.now() - 30_000).toISOString(),
      last_error: null,
    });
    // ANSI stripping kept lightweight — we just check key substrings.
    expect(out).toContain("tools.json");
    expect(out).toContain("sha256:0123456789ab…");
    expect(out).toMatch(/30s ago/);
    expect(out).not.toContain("last_error");
  });

  it("surfaces last_error on a second line so operators see the cause", () => {
    const out = renderEntry({
      kind: "ToolPolicy",
      digest: null,
      source_path: "/var/run/kars/toolpolicy/tools.json",
      loaded_at: "1970-01-01T00:00:00Z",
      last_error: "parse error: unexpected EOF",
    });
    expect(out).toContain("(no digest)");
    expect(out).toContain("(never loaded)");
    expect(out).toContain("last_error: parse error: unexpected EOF");
  });
});

describe("inspect — POLICY_KIND_LABEL", () => {
  it("maps every PolicyKind wire string the router emits today", () => {
    // Wire kinds emitted by
    // `inference-router/src/policy_status.rs::PolicyKind::as_str`:
    //   AgtProfile (Slice 1a) — controller-side AGT profile aggregate.
    //   ToolPolicy (Slice 1b/c) — compiled ToolPolicy artifacts.
    //   InferencePolicy (Slice 2a) — compiled InferencePolicy.
    //   Egress (future) — defensive carry-over.
    //   Memory (Slice 3a) — compiled KarsMemory binding.
    //
    // A partial-rollout cluster where the router is ahead of the CLI
    // still prints sensible labels for the kinds the CLI knows.
    for (const wire of [
      "ToolPolicy",
      "AgtProfile",
      "InferencePolicy",
      "Egress",
      "Memory",
    ]) {
      expect(POLICY_KIND_LABEL[wire]).toBeTypeOf("string");
      expect(POLICY_KIND_LABEL[wire]).not.toBe("");
    }
  });

  it("renders the Slice 3a `Memory` wire kind as `KarsMemory`", () => {
    // Slice 3a contract: the router emits `PolicyKind::Memory`
    // (as_str="Memory") for the compiled binding mounted at
    // `/etc/kars/memory/binding.json`. The CLI must display
    // the user-facing CRD name so `inspect` reads naturally next
    // to `kubectl get karsmemory`.
    expect(POLICY_KIND_LABEL["Memory"]).toBe("KarsMemory");
  });
});
