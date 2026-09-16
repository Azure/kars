// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
export const context = "kind-kars-budget-api";

class BudgetApiCommandFailure extends Error {
  constructor(error) {
    super("Disposable budget API assertion command failed", { cause: undefined });
    const message = String(error.stderr ?? "");
    this.budgetTokenPolicyDenied = error.status === 1
      && /(?:^|\n)The serviceaccounts ["']untrusted["'] is invalid:/.test(message)
      && /ValidatingAdmissionPolicy ['"]kars-inference-budget-token['"] with binding ['"]kars-inference-budget-token['"] denied request: Only kubelet node identities may obtain a Pod-bound governed-inference audience token(?:$|[\s"])/.test(message);
  }
}

export function isBudgetTokenPolicyDenial(error) {
  return error instanceof BudgetApiCommandFailure && error.budgetTokenPolicyDenied;
}

export function kubectl(args, input, publicSchema = false) {
  try {
    return execFileSync("kubectl", ["--context", context, "--request-timeout=20s", ...args], {
      cwd: root, encoding: "utf8", input: input === undefined ? undefined : JSON.stringify(input),
      stdio: ["pipe", "pipe", "pipe"], timeout: 30_000,
    });
  } catch (error) {
    if (publicSchema) {
      // Public schemas/readiness and synthetic authorization reviews only, never Secret/token commands.
      console.error(String(error.stderr ?? "").slice(0, 12_000));
    }
    throw new BudgetApiCommandFailure(error);
  }
}
