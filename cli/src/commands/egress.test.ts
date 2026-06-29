// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { egressCommand, parseDomainPort, unionEndpoint, removeHost } from "./egress.js";

/**
 * Lightweight CLI-shape tests for the S12.c `--sign` family. The full
 * sign flow is covered by `egress/sign.test.ts` (subprocess argv
 * construction is unit-tested there). Here we lock in:
 *
 *   - the new flags exist on the egress command,
 *   - `--sign` without `--enforce`/`--approve` is a hard error.
 *
 * We do NOT spin up real kubectl/oras/cosign here.
 */

function getOption(cmd: ReturnType<typeof egressCommand>, long: string) {
  return cmd.options.find((o) => o.long === long || o.short === long);
}

describe("egressCommand — --sign family wiring", () => {
  it("registers --sign", () => {
    const cmd = egressCommand();
    expect(getOption(cmd, "--sign")).toBeDefined();
  });

  it("registers --no-sign for forward compatibility", () => {
    const cmd = egressCommand();
    // commander represents --no-sign as a negation of --sign;
    // long form "--no-sign" is what the user types.
    const optNames = cmd.options.map((o) => o.long);
    expect(optNames).toContain("--no-sign");
  });

  it("registers --sign-mode <mode>", () => {
    const cmd = egressCommand();
    const opt = getOption(cmd, "--sign-mode");
    expect(opt).toBeDefined();
    expect(opt?.flags).toContain("<mode>");
  });

  it("registers --sign-key <ref>", () => {
    const cmd = egressCommand();
    expect(getOption(cmd, "--sign-key")).toBeDefined();
  });

  it("registers --registry <fqdn>", () => {
    const cmd = egressCommand();
    expect(getOption(cmd, "--registry")).toBeDefined();
  });

  it("registers --repository <repo>", () => {
    const cmd = egressCommand();
    expect(getOption(cmd, "--repository")).toBeDefined();
  });
  it("registers --emit-manifest <path>", () => {
    const cmd = egressCommand();
    const opt = getOption(cmd, "--emit-manifest");
    expect(opt).toBeDefined();
    expect(opt?.flags).toContain("<path>");
  });

  it("registers --force", () => {
    const cmd = egressCommand();
    expect(getOption(cmd, "--force")).toBeDefined();
  });
});

describe("egressCommand — --sign requires --enforce or --approve", () => {
  it("prints an error and sets exit code when --sign is used alone", async () => {
    const cmd = egressCommand();
    cmd.exitOverride();

    const logged: string[] = [];
    const realLog = console.log;
    console.log = (msg?: any) => {
      logged.push(String(msg ?? ""));
    };
    const prevExit = process.exitCode;
    process.exitCode = 0;

    try {
      await cmd.parseAsync(["demo-agent", "--sign"], { from: "user" });
    } catch {
      // commander may throw on action errors; we only care about output
    } finally {
      console.log = realLog;
    }

    const all = logged.join("\n");
    expect(all).toMatch(/--sign requires --enforce, --approve, or --deny/);
    expect(process.exitCode).toBe(1);
    process.exitCode = prevExit;
  });
});

describe("egressCommand — S12.g default-on sign + emit-manifest guards", () => {
  function captureLogs() {
    const logged: string[] = [];
    const realLog = console.log;
    console.log = (msg?: any) => {
      logged.push(String(msg ?? ""));
    };
    return {
      logs: logged,
      restore: () => {
        console.log = realLog;
      },
    };
  }

  it("--emit-manifest without --enforce/--approve errors", async () => {
    const cmd = egressCommand();
    cmd.exitOverride();
    const cap = captureLogs();
    const prev = process.exitCode;
    process.exitCode = 0;
    try {
      await cmd.parseAsync(
        ["demo-agent", "--emit-manifest", "./out.yaml"],
        { from: "user" },
      );
    } catch {
      /* commander */
    } finally {
      cap.restore();
    }
    expect(cap.logs.join("\n")).toMatch(/--emit-manifest requires --enforce, --approve, or --deny/);
    expect(process.exitCode).toBe(1);
    process.exitCode = prev;
  });

  it("--emit-manifest with --no-sign errors loudly", async () => {
    const cmd = egressCommand();
    cmd.exitOverride();
    const cap = captureLogs();
    const prev = process.exitCode;
    process.exitCode = 0;
    try {
      await cmd.parseAsync(
        ["demo-agent", "--enforce", "--no-sign", "--emit-manifest", "./m.yaml"],
        { from: "user" },
      );
    } catch {
      /* commander */
    } finally {
      cap.restore();
    }
    expect(cap.logs.join("\n")).toMatch(/--emit-manifest cannot be combined with --no-sign/);
    expect(process.exitCode).toBe(1);
    process.exitCode = prev;
  });
});

describe("parseDomainPort", () => {
  it("defaults the port to 443 when omitted", () => {
    expect(parseDomainPort("api.telegram.org")).toEqual({ host: "api.telegram.org", port: 443 });
  });
  it("parses an explicit host:port", () => {
    expect(parseDomainPort("example.com:8443")).toEqual({ host: "example.com", port: 8443 });
  });
  it("lowercases the host", () => {
    expect(parseDomainPort("API.Telegram.ORG")).toEqual({ host: "api.telegram.org", port: 443 });
  });
  it("honours a custom default port", () => {
    expect(parseDomainPort("redis.internal", 6379)).toEqual({ host: "redis.internal", port: 6379 });
  });
  it("rejects an empty value", () => {
    expect(() => parseDomainPort("   ")).toThrow(/must not be empty/);
  });
  it("rejects a URL (scheme/path)", () => {
    expect(() => parseDomainPort("https://api.telegram.org")).toThrow(/bare host/);
  });
  it("rejects an out-of-range port", () => {
    expect(() => parseDomainPort("example.com:70000")).toThrow(/out of range/);
  });
  it("treats a non-numeric suffix after ':' as part of the host (IPv6-ish guard)", () => {
    // No digits after the colon → not a port; whole string is the host.
    expect(parseDomainPort("weird:name")).toEqual({ host: "weird:name", port: 443 });
  });
});

describe("unionEndpoint / removeHost", () => {
  const base = [
    { host: "a.example.com", port: 443 },
    { host: "b.example.com", port: 443 },
  ];
  it("adds a new endpoint and keeps the list sorted", () => {
    const out = unionEndpoint(base, { host: "0.example.com", port: 443 });
    expect(out.map((e) => e.host)).toEqual(["0.example.com", "a.example.com", "b.example.com"]);
  });
  it("is idempotent for an existing host+port", () => {
    const out = unionEndpoint(base, { host: "a.example.com", port: 443 });
    expect(out).toHaveLength(2);
  });
  it("treats a different port on the same host as a distinct endpoint", () => {
    const out = unionEndpoint(base, { host: "a.example.com", port: 8443 });
    expect(out).toHaveLength(3);
    expect(out.filter((e) => e.host === "a.example.com")).toHaveLength(2);
  });
  it("removeHost drops every port for the host", () => {
    const withTwoPorts = [...base, { host: "a.example.com", port: 8443 }];
    const out = removeHost(withTwoPorts, "a.example.com");
    expect(out.map((e) => e.host)).toEqual(["b.example.com"]);
  });
  it("removeHost is case-insensitive and a no-op when absent", () => {
    expect(removeHost(base, "A.EXAMPLE.COM").map((e) => e.host)).toEqual(["b.example.com"]);
    expect(removeHost(base, "z.example.com")).toHaveLength(2);
  });
});

describe("unionEndpoint — port-less existing entries", () => {
  it("treats a port-less existing host as :443 for the dedupe check", () => {
    const raw = [{ host: "api.telegram.org" }]; // no port
    const out = unionEndpoint(raw, { host: "api.telegram.org", port: 443 });
    // Already considered present (port-less ⇒ 443), so no duplicate is added.
    expect(out).toHaveLength(1);
  });
  it("preserves a port-less entry when adding a different host", () => {
    const raw = [{ host: "keep.example.com" }];
    const out = unionEndpoint(raw, { host: "new.example.com", port: 443 });
    expect(out).toHaveLength(2);
    expect(out.find((e) => e.host === "keep.example.com")).toEqual({ host: "keep.example.com" });
  });
});

describe("removeHost — last-endpoint edge", () => {
  it("can reduce the baseline to empty (deny of the last host)", () => {
    const single = [{ host: "only.example.com", port: 443 }];
    expect(removeHost(single, "only.example.com")).toEqual([]);
  });
});
