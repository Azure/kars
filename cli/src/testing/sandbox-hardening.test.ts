// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Regression tests for the sandbox entrypoint hardening invariants.
// Plan item s7. Derived from the production incident on 2026-04-22 where
// a stale sandbox image lacked the `cp ... 2>/dev/null || true` wrapping
// and crash-looped on `cp: setting permissions ... Operation not permitted`
// under the image-level `chmod -R a-w` of `/opt/kars-plugin`.
//
// The tests run in two layers:
//   LAYER 1 — static: grep the entrypoint.sh / Dockerfile source for the
//     exact patterns that made the incident unrecoverable under `set -e`.
//   LAYER 2 — behavioural: spawn bash against a controlled tmpdir that
//     mimics `/opt/kars-plugin` (read-only source via `chmod -R a-w`)
//     and a writable destination; assert the real cp pattern exits 0.

import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const REPO_ROOT = join(__dirname, "..", "..", "..");
const ENTRYPOINT = join(REPO_ROOT, "sandbox-images", "openclaw", "entrypoint.sh");
const DOCKERFILE = join(REPO_ROOT, "sandbox-images", "openclaw", "Dockerfile");

// Extract the text of the first block between two anchor regexes.
function extractBlock(source: string, start: RegExp, end: RegExp): string {
  const startMatch = source.match(start);
  if (!startMatch) throw new Error(`start anchor not found: ${start}`);
  const after = source.slice(startMatch.index!);
  const endMatch = after.match(end);
  if (!endMatch) throw new Error(`end anchor not found: ${end}`);
  return after.slice(0, endMatch.index! + endMatch[0].length);
}

describe("sandbox entrypoint.sh hardening — static invariants (s7)", () => {
  const entrypoint = readFileSync(ENTRYPOINT, "utf8");

  it("uses `set -e` (strict mode — today's regression was caused by this)", () => {
    // Must be early in the script, before any command that could fail.
    expect(entrypoint).toMatch(/^\s*set -e(u|uo|uo pipefail)?\s*$/m);
  });

  it("plugin-install block opens with `rm -rf ... 2>/dev/null || true`", () => {
    // The block must start by wiping the prior install; failures there
    // (stale read-only files) must not abort the script.
    const block = extractBlock(
      entrypoint,
      /if \[ -d \/opt\/kars-plugin \]; then/,
      /^fi$/m,
    );
    expect(block).toMatch(/rm -rf "\$OPENCLAW_DIR\/extensions\/kars"[^\n]*\|\| true/);
  });

  it("every `cp` from /opt/kars-plugin/ tolerates EPERM (|| true)", () => {
    // The regression class: `cp` emits "Operation not permitted" when the
    // source is chmod -R a-w (Dockerfile hardening) on some kernels; cp
    // returns non-zero, and without `|| true` the script aborts under set -e.
    const block = extractBlock(
      entrypoint,
      /if \[ -d \/opt\/kars-plugin \]; then/,
      /^fi$/m,
    );
    // Every line containing a cp from the hardened source dirs.
    const cpLines = block.split("\n").filter((l) =>
      /^\s*cp\b/.test(l) &&
      (l.includes("/opt/kars-plugin") ||
        l.includes("/opt/clawhub-skills") ||
        l.includes('"$POLICY_SRC"')),
    );
    expect(cpLines.length).toBeGreaterThan(0);
    for (const line of cpLines) {
      expect(line, `cp without '|| true' would abort under set -e: ${line.trim()}`).toMatch(/\|\|\s*(true|:)/);
    }
  });

  it("every `cp` from /opt/kars-plugin/ uses --no-preserve=mode", () => {
    // Without --no-preserve=mode, cp copies the 444 source mode onto the
    // destination, so the NEXT restart's cp-over-existing-file fails.
    const block = extractBlock(
      entrypoint,
      /if \[ -d \/opt\/kars-plugin \]; then/,
      /^fi$/m,
    );
    const cpLines = block.split("\n").filter((l) =>
      /^\s*cp\b/.test(l) && l.includes("/opt/kars-plugin"),
    );
    for (const line of cpLines) {
      expect(line, `cp without --no-preserve=mode bakes in read-only mode: ${line.trim()}`)
        .toMatch(/--no-preserve=mode/);
    }
  });

  it("every `cp` in the plugin-install block suppresses stderr (2>/dev/null)", () => {
    // Not strictly required for correctness (|| true handles exit code), but
    // stderr noise ("Operation not permitted") breaks log-scraping alerts.
    const block = extractBlock(
      entrypoint,
      /if \[ -d \/opt\/kars-plugin \]; then/,
      /^fi$/m,
    );
    const cpLines = block.split("\n").filter((l) => /^\s*cp\b/.test(l));
    for (const line of cpLines) {
      expect(line, `cp without 2>/dev/null surfaces EPERM noise: ${line.trim()}`)
        .toMatch(/2>\/dev\/null/);
    }
  });

  it("code-integrity hardening block: every chmod/chown tolerates EPERM", () => {
    // Docker Desktop volume mounts sometimes return EPERM on fchmod; the
    // hardening is best-effort and must never abort the script.
    const block = extractBlock(
      entrypoint,
      /# ── Code integrity hardening/,
      /^fi$/m,
    );
    const lines = block.split("\n").filter((l) =>
      /^\s*(chown|chmod|find [^\n]*-exec chmod|find [^\n]*-exec chown)/.test(l),
    );
    expect(lines.length).toBeGreaterThan(3);
    for (const line of lines) {
      expect(line, `hardening op without '|| true' aborts script: ${line.trim()}`)
        .toMatch(/\|\|\s*(true|:)/);
    }
  });

  it("blanket chown -R sandbox:sandbox /sandbox tolerates EPERM", () => {
    // If this chown aborts, the entire hardening block below never runs —
    // silently leaving plugin code writable by the agent.
    expect(entrypoint).toMatch(
      /chown -R sandbox:sandbox \/sandbox[^\n]*\|\|\s*true/,
    );
  });
});

describe("sandbox Dockerfile — image-level hardening (s7)", () => {
  const dockerfile = readFileSync(DOCKERFILE, "utf8");

  it("makes /opt/kars-plugin read-only at image build time", () => {
    expect(dockerfile).toMatch(/chmod -R a-w [^\n]*\/opt\/kars-plugin/);
  });

  it("restores read/traverse bits after the a-w lockdown", () => {
    // Without a+rX, a subsequent `cp -r` from the source fails with "Permission denied"
    // because sandbox (UID 1000) cannot even enter the directory.
    expect(dockerfile).toMatch(/chmod -R a\+rX [^\n]*\/opt\/kars-plugin/);
  });
});

// Skip the behavioural layer on platforms without GNU coreutils (macOS uses BSD cp,
// which doesn't support --no-preserve=mode — the exact flag the sandbox depends on).
// The static tests above still run everywhere and catch the regression class.
function hasGnuCp(): boolean {
  const r = spawnSync("cp", ["--version"], { encoding: "utf8" });
  return r.status === 0 && /GNU coreutils/.test(r.stdout);
}
const BEHAVIOURAL_AVAILABLE =
  (existsSync("/bin/bash") || existsSync("/usr/bin/bash")) && hasGnuCp();
const describeIfBash = BEHAVIOURAL_AVAILABLE ? describe : describe.skip;

describeIfBash("sandbox entrypoint.sh — behavioural regression (s7)", () => {
  it("reproduces today's incident: cp under set -e against chmod -R a-w source exits 0", () => {
    // This is the exact scenario from 2026-04-22:
    //   * /opt/kars-plugin is chmod -R a-w (Dockerfile line 66)
    //   * entrypoint cps files out of it into the sandbox volume
    //   * Some hosts return EPERM on the implicit fchmod() after the write
    //   * Without `2>/dev/null || true`, set -e kills the container
    const dir = mkdtempSync(join(tmpdir(), "kars-s7-"));
    try {
      const src = join(dir, "opt-plugin");
      const dst = join(dir, "dst-extensions", "kars");
      execFileSync("mkdir", ["-p", src, dst]);
      writeFileSync(join(src, "package.json"), '{"name":"test"}\n');
      writeFileSync(join(src, "plugin.js"), "// test\n");
      // Mimic Dockerfile hardening — source is read-only, including for owner.
      execFileSync("chmod", ["-R", "a-w", src]);

      // Minimal script mirroring the real entrypoint's cp pattern. If the
      // real script regresses by dropping `|| true`, a corresponding test
      // above will fail. This layer proves the pattern itself works on the
      // host kernel.
      const script = `
set -e
cp --no-preserve=mode ${src}/package.json ${dst}/ 2>/dev/null || true
cp -r --no-preserve=mode ${src}/*.js ${dst}/ 2>/dev/null || true
echo OK
`;
      const result = spawnSync("/bin/bash", ["-c", script], { encoding: "utf8" });
      expect(result.status).toBe(0);
      expect(result.stdout.trim()).toBe("OK");
      // Destination files must exist and be readable.
      expect(existsSync(join(dst, "package.json"))).toBe(true);
      expect(existsSync(join(dst, "plugin.js"))).toBe(true);
    } finally {
      // Restore write perms before rm — source was chmod a-w.
      try { execFileSync("chmod", ["-R", "u+w", dir]); } catch { /* best-effort */ }
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("proves the regression: the SAME pattern without '|| true' aborts under set -e", () => {
    // Without this test, we'd have no signal that `|| true` is load-bearing.
    // Note: we cannot force an EPERM deterministically across kernels, so we
    // force a failure via a definitely-missing source file — which is the
    // same exit-code path cp takes on EPERM.
    const dir = mkdtempSync(join(tmpdir(), "kars-s7-neg-"));
    try {
      const dst = join(dir, "dst");
      execFileSync("mkdir", ["-p", dst]);

      const badScript = `
set -e
cp /nonexistent-source-file ${dst}/ 2>/dev/null
echo SHOULD_NOT_PRINT
`;
      const bad = spawnSync("/bin/bash", ["-c", badScript], { encoding: "utf8" });
      expect(bad.status).not.toBe(0);
      expect(bad.stdout).not.toContain("SHOULD_NOT_PRINT");

      const goodScript = `
set -e
cp /nonexistent-source-file ${dst}/ 2>/dev/null || true
echo RECOVERED
`;
      const good = spawnSync("/bin/bash", ["-c", goodScript], { encoding: "utf8" });
      expect(good.status).toBe(0);
      expect(good.stdout.trim()).toBe("RECOVERED");
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
});
