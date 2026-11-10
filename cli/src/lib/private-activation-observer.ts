// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { isDeepStrictEqual } from "node:util";
import { at, record } from "./private-activation.js";

const VERSION = "kars.azure.com/services-observer-version";

function remove(values: unknown, expected: Record<string, unknown>, defaultMode = false): void {
  if (!Array.isArray(values)) throw new Error("Malformed observer projection");
  const entries = values.map((value, index) => ({ value, index })).filter(({ value }) => at(value, "name") === expected.name);
  if (entries.length !== 1) throw new Error("Malformed observer projection");
  const entry = entries[0]!;
  const actual = structuredClone(record(entry.value));
  if (defaultMode && at(actual, "secret", "defaultMode") === 420) delete record(actual.secret).defaultMode;
  if (!isDeepStrictEqual(actual, expected)) throw new Error("Malformed observer projection");
  values.splice(entry.index, 1);
}

/** Matches the controller's exact router-only rendering, never arbitrary observer-looking fields. */
export function projectObserver(template: Record<string, unknown>): string | undefined {
  const fields = at(template, "metadata", "annotations");
  const version = at(fields, VERSION);
  if (version === undefined) return undefined;
  if (typeof version !== "string" || !version) throw new Error("Malformed observer projection");
  const pod = record(template.spec);
  remove(pod.volumes, { name: "service-observations", secret: { secretName: "router-services-observer", items: [
    { key: "observation-token", path: "observation-token" }, { key: "config.json", path: "config.json" },
  ] } }, true);
  remove(pod.volumes, { name: "service-observation-identity", secret: { secretName: "router-services-observer-identity",
    items: [{ key: "config.json", path: "config.json" }] } }, true);
  if (!Array.isArray(pod.containers)) throw new Error("Malformed observer projection");
  const routers = pod.containers.filter(container => at(container, "name") === "inference-router");
  if (routers.length !== 1) throw new Error("Malformed observer projection");
  const router = record(routers[0]);
  remove(router.env, { name: "KARS_SERVICE_OBSERVATION_VERSION", value: version });
  remove(router.volumeMounts, { name: "service-observations", mountPath: "/etc/kars/observations", readOnly: true });
  remove(router.volumeMounts, { name: "service-observation-identity", mountPath: "/etc/kars/observation-identity", readOnly: true });
  delete record(fields)[VERSION];
  return version;
}
