// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { at, canonical, read, record, reviewed, type Execute, type PrivateActivation } from "./private-activation.js";
import { refreshGuardRetirement, type GuardRetirementReview } from "./private-activation-guard-retirement.js";
import { observeWriterSettlement, settleWriterRetirement, type WriterSettlement } from "./private-activation-writer-settle.js";

const resource = "karscredentialgrants.kars.azure.com";

/** The captured review may be persisted before withdrawal and reused after an interrupted acknowledgement. */
export async function retireGrantWriters(
  run: Execute, existing: ReturnType<typeof record>, guardReview: GuardRetirementReview,
  settlement?: WriterSettlement,
  validateRetirement?: (execute: Execute, activation: PrivateActivation) => Promise<void>,
): Promise<{ activation: PrivateActivation; grant: ReturnType<typeof record> }> {
  const namespace = at(existing, "metadata", "namespace");
  if (typeof namespace !== "string" || reviewed(existing).name !== "workspace") throw new Error("Invalid writer retirement grant");
  const quiescentSpec = { ...record(existing.spec), writers: [] };
  const before = await read(run, resource, "workspace", namespace);
  if (reviewed(before).uid !== reviewed(existing).uid) throw new Error("Grant changed before writer retirement");
  if (canonical(before.spec) !== canonical(quiescentSpec)) {
    if (canonical(before.spec) !== canonical(existing.spec)
      || reviewed(before).resourceVersion !== reviewed(existing).resourceVersion) throw new Error("Grant changed before writer retirement");
    await run(["patch", resource, "workspace", "-n", namespace, "--type=merge", "-p", JSON.stringify({
      metadata: { uid: reviewed(before).uid, resourceVersion: reviewed(before).resourceVersion }, spec: quiescentSpec,
    })]);
  }
  const deadline = Date.now() + 120_000;
  for (;;) {
    const current = await read(run, resource, "workspace", namespace);
    if (reviewed(current).uid !== reviewed(existing).uid || canonical(current.spec) !== canonical(quiescentSpec)) {
      throw new Error("Grant changed while retiring prior private writer authority");
    }
    if (settlement) await observeWriterSettlement(run, guardReview.activation, settlement);
    const conditions = at(current, "status", "conditions");
    if (at(current, "status", "observedGeneration") === at(current, "metadata", "generation")
      && Array.isArray(conditions) && conditions.some(c => at(c, "type") === "WriterReady" && at(c, "status") === "False")) {
      const inventory = record(JSON.parse(await run(["get", "roles,rolebindings,clusterroles,clusterrolebindings",
        "--all-namespaces", "--chunk-size=0", "-o", "json"])));
      if (!Array.isArray(inventory.items) || at(inventory, "metadata", "continue")) {
        throw new Error("Private authority retirement inventory is incomplete");
      }
      if (!inventory.items.some(object =>
        at(object, "metadata", "annotations", "kars.azure.com/credential-grant-owner") === reviewed(existing).uid)) {
        const activation = await refreshGuardRetirement(run, guardReview, settlement
          ? value => settleWriterRetirement(run, value, settlement) : undefined, validateRetirement);
        if (activation) return { activation, grant: current };
      }
    }
    if (Date.now() >= deadline) throw new Error("Prior writer authority retirement is still pending; no new activation was published");
    await new Promise(resolve => setTimeout(resolve, 500));
  }
}
