// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { DatapathWitness } from "./types/operations";

export const WITNESS_ENABLE_COMMAND = 'helm upgrade --install kars-datapath-witness ./deploy/helm/kars-datapath-witness --namespace kars-system --kube-context "${KARS_CONTEXT:?Set the reviewed kube context}" --values "${WITNESS_VALUES:?Set the reviewed witness values file}" --set enabled=true --wait --timeout 10m';
export const WITNESS_DISABLE_COMMAND = 'helm upgrade kars-datapath-witness ./deploy/helm/kars-datapath-witness --namespace kars-system --kube-context "${KARS_CONTEXT:?Set the reviewed kube context}" --reuse-values --set enabled=false --wait --timeout 10m';

const labels = {
  missing: "No report / installation unknown",
  pending: "Requested on / awaiting report",
  disabled: "Requested off / verify removal",
  stale: "Stale report",
  invalid: "Invalid report or intent",
  unavailable: "Witness status unavailable",
  observed: "Fresh partial sample",
  empty: "Empty sample / coverage unproven",
  legacy: "Legacy report / operator review",
};

export function witnessPresentation(witness: DatapathWitness | null, now = Date.now()) {
  const generated = Date.parse(witness?.generated_at ?? "");
  const reportedSample = witness?.state === "observed" || witness?.state === "empty";
  const fresh = reportedSample && Number.isFinite(generated)
    && now - generated <= 180_000 && generated - now <= 30_000;
  const state = reportedSample && !fresh ? "stale" : witness?.state;
  return {
    label: state && Object.hasOwn(labels, state) ? labels[state] : "Observation health unknown",
    fresh,
    state,
    diagnostic: reportedSample && !fresh
      ? "The sample is no longer fresh. Reload to read current status; old observations are not live evidence."
      : witness?.diagnostic ?? "This server does not report observation health. Installation cannot be inferred from a report or its absence.",
    requested: witness?.requested_enabled === true ? "On" : witness?.requested_enabled === false ? "Off" : "Unknown",
  };
}

export async function copyWitnessCommand(
  action: "enable" | "disable",
  clipboard: Pick<Clipboard, "writeText"> | undefined,
): Promise<string> {
  if (!clipboard) throw new Error("Clipboard unavailable");
  await clipboard.writeText(action === "enable" ? WITNESS_ENABLE_COMMAND : WITNESS_DISABLE_COMMAND);
  return "Command copied. No cluster change was made; a cluster operator must review and run it.";
}
