// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { devNull } from "node:os";
import { describe, expect, it, vi } from "vitest";
import { readSecretMetadata, type Execute } from "./private-activation.js";

describe("private Secret metadata projection", () => {
  it("uses the real kubectl printer without returning Secret values or contacting a cluster", async () => {
    const execute: Execute = async args => {
      expect(args.slice(0, 5)).toEqual(["get", "secret", "metadata-fixture", "-n", "reviewed"]);
      const format = args[args.indexOf("-o") + 1]!;
      return execFileSync("kubectl", [
        "--kubeconfig", devNull, "--server", "http://127.0.0.1:1", "--request-timeout=2s",
        "create", "secret", "generic", "metadata-fixture", "--namespace", "reviewed",
        "--from-literal=fixture=public-test-value", "--dry-run=client", "-o", format,
      ], { encoding: "utf8", timeout: 10_000, windowsHide: true });
    };
    const metadata = await readSecretMetadata(execute, "metadata-fixture", "reviewed");
    expect(metadata.name).toBe("metadata-fixture");
    expect(metadata.namespace).toBe("reviewed");
    expect(metadata).not.toHaveProperty("data");
    expect(JSON.stringify(metadata)).not.toContain("public-test-value");
  }, 15_000);

  it.each([{}, null, "metadata", [], [null], [[]], ["metadata"], [{}, {}]])(
    "rejects malformed or ambiguous metadata projection %j", async value => {
      await expect(readSecretMetadata(async () => JSON.stringify(value), "secret", "namespace")).rejects.toThrow();
    },
  );

  it("permits absence only for explicitly optional lookups", async () => {
    const execute = vi.fn(async (_args: readonly string[]) => "");
    await expect(readSecretMetadata(execute, "secret", "namespace")).rejects.toThrow("missing");
    expect(execute.mock.calls[0]?.[0]).not.toContain("--ignore-not-found");
    await expect(readSecretMetadata(execute, "secret", "namespace", true)).resolves.toBeUndefined();
    expect(execute.mock.calls[1]?.[0]).toContain("--ignore-not-found");
  });

  it("does not disguise authorization or transport errors as absent optional material", async () => {
    const failure = new Error("fixture API denied");
    const execute: Execute = async () => { throw failure; };
    await expect(readSecretMetadata(execute, "secret", "namespace", true)).rejects.toBe(failure);
  });
});
