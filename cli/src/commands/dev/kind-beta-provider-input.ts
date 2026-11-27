// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "../../lib/private-activation.js";
import { waitForWorkspaceProviders } from "../../lib/workspace-provider-setup.js";

type ObjectMap = Record<string, any>;
const name = "kars-inference-providers", namespace = "kars-system";

/** Fill the installed store only after its real workspace authority is ready. */
export async function publishKindBetaProviderInput(input: ObjectMap, execute: Execute, write: Execute = execute): Promise<void> {
  if (input.apiVersion !== "v1" || input.kind !== "Secret" || input.type !== "Opaque"
    || input.metadata?.name !== name || input.metadata.namespace !== namespace
    || input.metadata.uid || input.metadata.resourceVersion || input.metadata.deletionTimestamp
    || input.metadata.ownerReferences || input.metadata.finalizers) throw new Error("Invalid fresh provider input");
  const data: Record<string, string> = {};
  for (const field of ["data", "stringData"]) for (const [key, value] of Object.entries(input[field] ?? {})) {
    if (typeof value !== "string" || Object.hasOwn(data, key)
      || key !== "COPILOT_GITHUB_TOKEN" && !/^KARS_PROVIDER_[A-Z0-9_]+_(ENDPOINT|API_KEY|TOKEN|MODELS)$/.test(key)) {
      throw new Error("Invalid or duplicate provider input key");
    }
    data[key] = field === "data" ? value : Buffer.from(value).toString("base64");
  }
  if (!Object.keys(data).length) throw new Error("Provider bootstrap requires credential inputs");
  await waitForWorkspaceProviders(execute, namespace);
  const grant = JSON.parse(await execute(["get", "karscredentialgrants.kars.azure.com", "workspace", "-n", namespace, "-o", "json"]));
  const store = JSON.parse(await execute(["get", "secret", name, "-n", namespace, "-o", "json"]));
  const workspace = JSON.parse(await execute(["get", "namespace", namespace, "-o", "json"]));
  const stores = grant.spec?.integrationStores?.filter((entry: ObjectMap) => entry.secret?.name === name);
  if (!grant.metadata?.uid || grant.metadata.deletionTimestamp || grant.spec?.enabled !== true
    || !workspace.metadata?.uid || workspace.metadata.deletionTimestamp || grant.spec.workspaceUid !== workspace.metadata.uid
    || grant.status?.phase !== "Ready" || !grant.metadata.generation || grant.status.observedGeneration !== grant.metadata.generation
    || grant.status.integrationError || !store.metadata?.uid || !store.metadata.resourceVersion || store.metadata.deletionTimestamp
    || store.type !== "Opaque" || store.metadata.annotations?.["kars.azure.com/credential-store-grant-uid"] !== grant.metadata.uid
    || stores?.length !== 1 || stores[0].secret.uid !== store.metadata.uid || stores[0].purpose !== "providers") {
    throw new Error("Provider store authority changed; retained all existing credentials");
  }
  if (Object.keys(store.data ?? {}).length || Object.keys(store.stringData ?? {}).length) {
    throw new Error("Provider store was populated concurrently; retained all existing credentials");
  }
  // A failed compare-and-swap is not retried: this is a fresh, single-use install.
  await write(["patch", "secret", name, "-n", namespace, "--type=json", "--patch-file=/dev/stdin"], JSON.stringify([
    { op: "test", path: "/metadata/uid", value: store.metadata.uid },
    { op: "test", path: "/metadata/resourceVersion", value: store.metadata.resourceVersion },
    { op: "add", path: "/data", value: data },
  ]));
  const installed = JSON.parse(await execute(["get", "secret", name, "-n", namespace, "-o", "json"]));
  if (installed.metadata?.uid !== store.metadata.uid || installed.metadata?.deletionTimestamp
    || Object.keys(installed.data ?? {}).length !== Object.keys(data).length
    || Object.entries(data).some(([key, value]) => installed.data?.[key] !== value)) {
    throw new Error("Installed provider inputs changed; inspect retained setup evidence");
  }
  await waitForWorkspaceProviders(execute, namespace);
}
