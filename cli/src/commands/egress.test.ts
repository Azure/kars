import { describe, it, expect } from "vitest";
import { egressCommand } from "./egress.js";

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
    expect(all).toMatch(/--sign requires --enforce or --approve/);
    expect(process.exitCode).toBe(1);
    process.exitCode = prevExit;
  });
});
