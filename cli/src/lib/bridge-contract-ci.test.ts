// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { parse } from "yaml";

function mapping(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Workflow contract requires a mapping");
  }
  return value as Record<string, unknown>;
}

function workflow(name: string): Record<string, unknown> {
  return mapping(parse(readFileSync(new URL(`../../../.github/workflows/${name}`, import.meta.url), "utf8")));
}

function jobSteps(document: Record<string, unknown>, name: string): Record<string, unknown>[] {
  const value = mapping(mapping(document.jobs)[name]).steps;
  if (!Array.isArray(value)) throw new Error("Workflow contract requires steps");
  return value.map(mapping);
}

describe("permanent core and Bridge CI boundary", () => {
  it("always reports component acceptance and includes every component job", () => {
    const bridge = workflow("bridge-ci.yml");
    const events = mapping(bridge.on);
    expect(mapping(events.pull_request).paths).toBeUndefined();
    expect(mapping(events.pull_request)["paths-ignore"]).toBeUndefined();
    expect(mapping(events.push).paths).toBeUndefined();
    const jobs = mapping(bridge.jobs);
    const aggregate = mapping(jobs["bridge-required-gates"]);
    const expected = Object.keys(jobs).filter(name => name !== "bridge-required-gates").sort();
    expect(aggregate.needs).toEqual(expected);
    for (const name of expected) {
      expect(mapping(jobs[name])["continue-on-error"], name).toBeUndefined();
      for (const step of jobSteps(bridge, name)) {
        expect(step["continue-on-error"], name).toBeUndefined();
      }
    }
    expect(aggregate.if).toBe("always()");
    expect(mapping(aggregate.env).COMPONENT_RESULTS).toBe("${{ toJSON(needs) }}");
    expect(jobSteps(bridge, "bridge-required-gates").some(step =>
      step.run === "python3 ci/bridge_component_results.py")).toBe(true);
  });

  it("builds core Rust and CLI and exercises core Kind with Bridge physically absent", () => {
    const core = workflow("ci.yml");
    for (const name of ["build-rust", "cli-build", "e2e-kind"]) {
      const steps = jobSteps(core, name);
      const checkout = steps.find(step => String(step.uses).startsWith("actions/checkout@"));
      expect(checkout, name).toBeDefined();
      const options = mapping(checkout?.with);
      expect(options["sparse-checkout-cone-mode"], name).toBe(false);
      expect(String(options["sparse-checkout"]).trim().split("\n"), name).toEqual(["/*", "!/bridge/"]);
      const guard = steps.find(step => step.name === "Require standalone core checkout");
      expect(guard?.run, name).toContain("test ! -e bridge");
      expect(guard?.["working-directory"], name).toBe(".");
    }
    expect(jobSteps(core, "cli-build").some(step => step.run === "npm ci")).toBe(true);
    expect(jobSteps(core, "changes").some(step => String(step.run)
      .includes("ci/bridge_contracts.py --output code"))).toBe(true);
    expect(jobSteps(core, "e2e-kind").some(step => String(step.run)
      .includes("ci/bridge_contracts.py --output run --core-only"))).toBe(true);
    expect(mapping(mapping(core.jobs)["e2e-kind"]).if).toBe("needs.changes.outputs.code == 'true'");
  });

  it("always creates the native contract status for PRs and qualifies merged tips", () => {
    const native = workflow("bridge-native.yml");
    const events = mapping(native.on);
    const pullRequest = mapping(events.pull_request);
    expect(pullRequest.branches).toEqual(expect.arrayContaining(["main", "kars-bridge"]));
    expect(pullRequest.paths).toBeUndefined();
    expect(pullRequest["paths-ignore"]).toBeUndefined();
    expect(mapping(events.push).branches).toEqual(expect.arrayContaining(["main", "kars-bridge"]));
    const scope = mapping(mapping(native.jobs)["contract-scope"]);
    expect(mapping(scope.outputs).required).toBe("${{ steps.scope.outputs.required }}");
    const steps = jobSteps(native, "contract-scope");
    expect(steps.some(step => String(step.run).includes("ci/bridge_contracts.py"))).toBe(true);
    const checkout = steps.find(step => String(step.uses).startsWith("actions/checkout@"));
    expect(mapping(checkout?.with)["fetch-depth"]).toBe(0);
    const api = mapping(mapping(native.jobs)["api-admission"]);
    const strategy = mapping(api.strategy);
    expect(strategy["fail-fast"]).toBe(false);
    expect(mapping(strategy.matrix).cold_install).toEqual([1, 2, 3]);
    const apiSteps = jobSteps(native, "api-admission");
    expect(apiSteps.some(step => String(step.run).includes("npm ci --prefix .native/core/cli"))).toBe(true);
    const artifact = apiSteps.find(step => String(step.uses).startsWith("actions/upload-artifact@"));
    expect(mapping(artifact?.with).name).toBe("native-api-evidence-${{ matrix.cold_install }}");
  });

  it("cannot turn skipped or failed required native jobs into a passing aggregate", () => {
    const native = workflow("bridge-native.yml");
    const jobs = mapping(native.jobs);
    for (const name of ["api-admission", "native-runtime"]) {
      const job = mapping(jobs[name]);
      expect(job.needs).toBe("contract-scope");
      expect(job.if).toBe("needs.contract-scope.outputs.required == 'true'");
    }
    const aggregate = mapping(jobs["native-required-gates"]);
    expect(aggregate.needs).toEqual(["contract-scope", "api-admission", "native-runtime"]);
    expect(aggregate.if).toBe("always()");
    const env = mapping(aggregate.env);
    expect(env.SCOPE_RESULT).toBe("${{ needs.contract-scope.result }}");
    expect(env.NATIVE_REQUIRED).toBe("${{ needs.contract-scope.outputs.required }}");
    expect(env.API_RESULT).toBe("${{ needs.api-admission.result }}");
    expect(env.RUNTIME_RESULT).toBe("${{ needs.native-runtime.result }}");
    const steps = jobSteps(native, "native-required-gates");
    expect(steps.some(step => step.run === "bash ci/bridge-contract-result.sh"
      && step["working-directory"] === ".")).toBe(true);
  });
});
