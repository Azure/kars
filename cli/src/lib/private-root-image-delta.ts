// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { at, canonical, record } from "./private-activation.js";

type ObjectValue = ReturnType<typeof record>;
const FAILURE = "Private single-root image delta contains unrelated or unreviewed changes";

function imageIndex(spec: ObjectValue): number {
  const containers = at(spec, "template", "spec", "containers");
  if (!Array.isArray(containers)) throw new Error(FAILURE);
  const indices = containers.flatMap((value, index) => at(value, "name") === "controller" ? [index] : []);
  if (indices.length !== 1) throw new Error(FAILURE);
  return indices[0]!;
}
/** New reviews must retain first-install authority before retiring an existing governed router. */
export function assertFirstInstallRouterContinuity(originalSpec: ObjectValue, targetSpec: ObjectValue): void {
  const env = (spec: ObjectValue): unknown[] => {
    const index = imageIndex(spec);
    const containers = at(spec, "template", "spec", "containers") as ObjectValue[];
    const entries = containers[index]!.env ?? [];
    if (!Array.isArray(entries)) throw new Error(FAILURE);
    return entries;
  };
  const before = env(originalSpec), after = env(targetSpec);
  const named = (entries: unknown[], name: string) => entries.filter(value => at(value, "name") === name);
  if (!named(before, "KARS_INFERENCE_BUDGET_ENABLED").some(value => at(value, "value") === "true")) return;
  if (canonical(named(before, "INFERENCE_ROUTER_IMAGE")) === canonical(named(after, "INFERENCE_ROUTER_IMAGE"))) return;
  const literal = (entries: unknown[], name: string): string => {
    const matches = named(entries, name);
    if (matches.length !== 1) throw new Error(FAILURE);
    const entry = record(matches[0]);
    if (Object.keys(entry).sort().join(",") !== "name,value" || typeof entry.value !== "string") throw new Error(FAILURE);
    return entry.value;
  };
  const baseline = "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST";
  const original = literal(before, named(before, baseline).length ? baseline : "KARS_INFERENCE_BUDGET_ROUTER_DIGEST");
  if (!/^sha256:[a-f0-9]{64}$/.test(original)) throw new Error(FAILURE);
  if (!named(after, baseline).length) {
    throw new Error(`Governed router migration must retain ${baseline}=${original} before retirement; preserve the original first-install verifier`);
  }
  if (literal(after, baseline) !== original) throw new Error(FAILURE);
}

export function assertRootImageDelta(originalSpec: ObjectValue, targetSpec: ObjectValue): void {
  const expected = structuredClone(originalSpec);
  const index = imageIndex(expected), targetIndex = imageIndex(targetSpec);
  const oldContainers = at(expected, "template", "spec", "containers") as ObjectValue[];
  const newContainers = at(targetSpec, "template", "spec", "containers") as ObjectValue[];
  const before = oldContainers[index]!, after = newContainers[targetIndex]!;
  const immutable = (value: unknown): value is string => typeof value === "string" && /^[^@\s]+@sha256:[a-f0-9]{64}$/.test(value);
  if (index !== targetIndex || !immutable(before.image) || !immutable(after.image)) throw new Error(FAILURE);
  let changed = before.image !== after.image;
  before.image = after.image;
  const shippingImages = new Set(["HERMES_RUNTIME_IMAGE", "SANDBOX_IMAGE", "INFERENCE_ROUTER_IMAGE", "KARS_CONFORMANCE_RUNNER_IMAGE"]);
  const oldEnv = before.env ?? [], newEnv = after.env ?? [];
  if (!Array.isArray(oldEnv) || !Array.isArray(newEnv)) throw new Error(FAILURE);
  const routerImage = (env: unknown[]) => at(env.find(value => at(value, "name") === "INFERENCE_ROUTER_IMAGE"), "value");
  const oldRouter = routerImage(oldEnv), newRouter = routerImage(newEnv);
  const pinName = "KARS_INFERENCE_BUDGET_ROUTER_DIGEST";
  const oldPin = oldEnv.find(value => at(value, "name") === pinName);
  const newPin = newEnv.find(value => at(value, "name") === pinName);
  const baselineName = "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST";
  const baselineIndex = newEnv.findIndex(value => at(value, "name") === baselineName);
  if (!oldEnv.some(value => at(value, "name") === baselineName) && baselineIndex !== -1) {
    const baseline = record(newEnv[baselineIndex]), originalPin = record(oldPin);
    const governed = record(oldEnv.find(value => at(value, "name") === "KARS_INFERENCE_BUDGET_ENABLED"));
    if ([baseline, originalPin, governed].some(value => Object.keys(value).sort().join(",") !== "name,value")
      || governed.value !== "true" || !immutable(oldRouter)
      || originalPin.value !== oldRouter.split("@")[1]
      || baseline.value !== originalPin.value) throw new Error(FAILURE);
    // With no explicit baseline, the old governed pin is already the first-install verifier.
    // Declare that same authority; the normal delta checks still bind every existing entry.
    oldEnv.splice(baselineIndex, 0, structuredClone(baseline));
  }
  if (oldEnv.length !== newEnv.length) throw new Error(FAILURE);
  if (oldRouter !== newRouter || canonical(oldPin ?? null) !== canonical(newPin ?? null)) {
    const governed = oldEnv.some(value => at(value, "name") === "KARS_INFERENCE_BUDGET_ENABLED"
      && at(value, "value") === "true");
    if (governed || oldPin !== undefined || newPin !== undefined) {
      const originalPin = record(oldPin), targetPin = record(newPin);
      if (!immutable(oldRouter) || !immutable(newRouter)
        || Object.keys(originalPin).sort().join(",") !== "name,value"
        || Object.keys(targetPin).sort().join(",") !== "name,value"
        || originalPin.value !== oldRouter.split("@")[1]
        || targetPin.value !== newRouter.split("@")[1]) throw new Error(FAILURE);
      originalPin.value = targetPin.value;
    }
  }
  const names = new Set<string>();
  for (const [position, value] of oldEnv.entries()) {
    const original = record(value), target = record(newEnv[position]);
    const name = original.name;
    if (typeof name !== "string" || !name || names.has(name) || target.name !== name) throw new Error(FAILURE);
    names.add(name);
    if (!shippingImages.has(name)) continue;
    // A retained adoption-default runner must not block unrelated image migrations.
    if (name === "KARS_CONFORMANCE_RUNNER_IMAGE" && canonical(original) === canonical(target)) continue;
    if (Object.keys(original).sort().join(",") !== "name,value"
      || Object.keys(target).sort().join(",") !== "name,value"
      || !immutable(original.value) || !immutable(target.value)) throw new Error(FAILURE);
    changed ||= original.value !== target.value;
    original.value = target.value;
  }
  if (!changed) throw new Error("Migration requires a changed immutable controller or existing shipping runtime or evaluation runner image");
  if (canonical(expected) !== canonical(targetSpec)) throw new Error(FAILURE);
}
