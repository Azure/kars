// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
export const context = "kind-kars-budget-api";

export function kubectl(args, input, publicSchema = false) {
  try {
    return execFileSync("kubectl", ["--context", context, "--request-timeout=20s", ...args], {
      cwd: root, encoding: "utf8", input: input === undefined ? undefined : JSON.stringify(input),
      stdio: ["pipe", "pipe", "pipe"], timeout: 30_000,
    });
  } catch (error) {
    if (publicSchema) {
      // Only public CRD/VAP creation and CRD readiness opt in, never Secret/token commands.
      console.error(String(error.stderr ?? "").slice(0, 12_000));
    }
    throw new Error("Disposable budget API assertion command failed", { cause: undefined });
  }
}
