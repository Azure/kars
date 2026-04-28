import { describe, it, expect } from "vitest";
import { __test, MIGRATE_MODES } from "./migrate.js";

describe("migrateCommand — validateMode", () => {
  it("accepts each known mode without an upstreamRef (except overlay)", () => {
    for (const m of MIGRATE_MODES) {
      if (m === "overlay") continue;
      expect(__test.validateMode(m, undefined)).toEqual([]);
    }
  });

  it("requires --upstream-ref for overlay", () => {
    const errs = __test.validateMode("overlay", undefined);
    expect(errs.length).toBe(1);
    expect(errs[0]).toContain("upstream-ref");
  });

  it("accepts overlay with a non-empty upstreamRef", () => {
    expect(__test.validateMode("overlay", "legacy-agent")).toEqual([]);
  });

  it("rejects --upstream-ref for non-overlay modes", () => {
    const errs = __test.validateMode("translate", "stray");
    expect(errs.length).toBeGreaterThan(0);
    expect(errs[0]).toContain("only meaningful for 'overlay'");
  });

  it("rejects empty-string upstreamRef even for overlay", () => {
    const errs = __test.validateMode("overlay", "");
    // Both 'required' and 'non-empty' fire — we just need a clear error.
    expect(errs.length).toBeGreaterThan(0);
    expect(errs.join(" ")).toMatch(/upstream-ref/);
  });

  it("rejects unknown mode strings", () => {
    // Cast through unknown to bypass the literal-type guard at the test edge.
    const errs = __test.validateMode("bogus" as unknown as "overlay", undefined);
    expect(errs[0]).toContain("invalid mode");
  });
});

describe("migrateCommand — buildModePatch", () => {
  it("emits sigsAgentSandbox + null upstreamSandboxRef for native", () => {
    expect(__test.buildModePatch("off", undefined)).toEqual({
      spec: {
        upstreamCompatibility: {
          sigsAgentSandbox: "off",
          upstreamSandboxRef: null,
        },
      },
    });
  });

  it("emits the upstream ref object for overlay", () => {
    expect(__test.buildModePatch("overlay", "legacy-agent")).toEqual({
      spec: {
        upstreamCompatibility: {
          sigsAgentSandbox: "overlay",
          upstreamSandboxRef: { name: "legacy-agent" },
        },
      },
    });
  });

  it("ignores upstreamRef for translate/observe (sets it to null)", () => {
    for (const m of ["translate", "observe"] as const) {
      const p = __test.buildModePatch(m, "should-be-ignored");
      expect(p.spec.upstreamCompatibility.upstreamSandboxRef).toBeNull();
      expect(p.spec.upstreamCompatibility.sigsAgentSandbox).toBe(m);
    }
  });

  it("uses null (not undefined) for upstreamSandboxRef removal — RFC 7396", () => {
    // JSON merge patch (RFC 7396) deletes a field on `null`. Undefined
    // would JSON-stringify to a missing key, leaving a stale value
    // behind on the server. This test guards that semantic.
    const patch = __test.buildModePatch("off", undefined);
    expect(JSON.stringify(patch)).toContain('"upstreamSandboxRef":null');
  });
});

describe("migrateCommand — readCurrentMode", () => {
  it("defaults to off + null when spec is missing", () => {
    expect(__test.readCurrentMode(undefined)).toEqual({ mode: "off", upstreamRef: null });
    expect(__test.readCurrentMode({})).toEqual({ mode: "off", upstreamRef: null });
  });

  it("reads overlay + upstream ref", () => {
    expect(
      __test.readCurrentMode({
        upstreamCompatibility: {
          sigsAgentSandbox: "overlay",
          upstreamSandboxRef: { name: "legacy" },
        },
      }),
    ).toEqual({ mode: "overlay", upstreamRef: "legacy" });
  });

  it("falls back to off on unknown mode strings", () => {
    expect(
      __test.readCurrentMode({
        upstreamCompatibility: { sigsAgentSandbox: "bogus" },
      }),
    ).toEqual({ mode: "off", upstreamRef: null });
  });

  it("ignores upstreamSandboxRef when mode is not overlay", () => {
    // The CRD doc says the field is *ignored* outside overlay; the
    // reader still surfaces it (so noop detection sees stale refs)
    // but the controller will not act on it.
    const out = __test.readCurrentMode({
      upstreamCompatibility: {
        sigsAgentSandbox: "translate",
        upstreamSandboxRef: { name: "stray" },
      },
    });
    expect(out.mode).toBe("translate");
    expect(out.upstreamRef).toBe("stray");
  });
});

describe("migrateCommand — summariseTransition", () => {
  it("flags noop when mode + ref unchanged", () => {
    const out = __test.summariseTransition(
      { mode: "off", upstreamRef: null },
      { mode: "off", upstreamRef: undefined },
    );
    expect(out.noop).toBe(true);
    expect(out.message).toContain("native");
    expect(out.message).toContain("already in target state");
  });

  it("flags noop for matching overlay+ref", () => {
    const out = __test.summariseTransition(
      { mode: "overlay", upstreamRef: "legacy" },
      { mode: "overlay", upstreamRef: "legacy" },
    );
    expect(out.noop).toBe(true);
  });

  it("detects ref-only change (overlay → overlay with new upstream)", () => {
    const out = __test.summariseTransition(
      { mode: "overlay", upstreamRef: "v1" },
      { mode: "overlay", upstreamRef: "v2" },
    );
    expect(out.noop).toBe(false);
    expect(out.message).toContain("'v2'");
  });

  it("renders 'native → overlay' for the canonical adoption story", () => {
    const out = __test.summariseTransition(
      { mode: "off", upstreamRef: null },
      { mode: "overlay", upstreamRef: "legacy" },
    );
    expect(out.noop).toBe(false);
    expect(out.message).toBe("native → overlay (upstream sandbox 'legacy')");
  });

  it("renders 'overlay → native' for the rollback story", () => {
    const out = __test.summariseTransition(
      { mode: "overlay", upstreamRef: "legacy" },
      { mode: "off", upstreamRef: undefined },
    );
    expect(out.message).toBe("overlay → native");
  });
});

describe("migrateCommand — modeDisplay", () => {
  it("renders 'off' as 'native'", () => {
    expect(__test.modeDisplay("off")).toBe("native");
  });

  it("passes through other modes verbatim", () => {
    expect(__test.modeDisplay("overlay")).toBe("overlay");
    expect(__test.modeDisplay("translate")).toBe("translate");
    expect(__test.modeDisplay("observe")).toBe("observe");
  });

  it("renders null/undefined as 'native' (matches default)", () => {
    expect(__test.modeDisplay(null)).toBe("native");
    expect(__test.modeDisplay(undefined)).toBe("native");
  });
});
