// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const harness = fileURLToPath(new URL("../../../tests/e2e/credential-sources.sh", import.meta.url));

describe("credential-source E2E cleanup bounds", () => {
  it.each([0, 1])("propagates policy deletion exit %s without hiding failure behind diagnostics", exit => {
    const result = spawnSync("bash", ["-c", `
      set -euo pipefail
      source "$1"
      fail() { printf 'FAIL: %s\\n' "$*" >&2; }
      kubectl() {
        printf 'CALL: %s\\n' "$*" >&2
        if [ "$3" = delete ]; then return "$POLICY_EXIT"; fi
        return 1
      }
      cleanup_credential_source_policy
    `, "credential-cleanup", harness], {
      encoding: "utf8", timeout: 5_000,
      env: { ...process.env, POLICY_EXIT: String(exit) },
    });
    expect(result.error).toBeUndefined();
    expect(result.status).toBe(exit);
    const calls = result.stderr.split("\n").filter(line => line.startsWith("CALL:"));
    expect(calls[0]).toBe(
      "CALL: --context kind-kars-e2e delete inferencepolicy e2e-source-inference -n kars-system --timeout=90s --request-timeout=20s",
    );
    expect(calls).toHaveLength(exit ? 4 : 1);
    expect(calls.every(call => call.includes("--context kind-kars-e2e") && call.includes("--request-timeout=20s")))
      .toBe(true);
    expect(result.stderr.includes("FAIL:")).toBe(exit !== 0);
    expect(calls.some(call => call.includes("get secret"))).toBe(false);
  });
});
