// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import path from "node:path";
import type { BetaExecute, BetaTools } from "./kind-beta.js";

/** Only safe, locally authored diagnostics may cross the concierge boundary. */
export class KindInventoryError extends Error {}

export async function listKindClusters(tools: Pick<BetaTools, "kind" | "runtime" | "env">, execute: BetaExecute): Promise<string[]> {
  const podman = tools.env.KIND_EXPERIMENTAL_PROVIDER === "podman" || path.basename(tools.runtime) === "podman";
  try {
    let names: unknown[];
    if (podman) {
      // Podman 6 changed template .Labels to a slice; Kind's index template fails.
      // JSON retains the label map, including stopped nodes that must not be adopted.
      const { stdout } = await execute(tools.runtime, ["ps", "-a", "--filter", "label=io.x-k8s.kind.cluster", "--format", "json"], { env: tools.env, stdio: "pipe" });
      const rows: unknown = JSON.parse(stdout);
      if (!Array.isArray(rows)) throw new Error("Invalid inventory");
      names = rows.map((row: unknown) => {
        if (!row || typeof row !== "object" || !("Labels" in row) || !row.Labels || typeof row.Labels !== "object"
          || Array.isArray(row.Labels) || !("io.x-k8s.kind.cluster" in row.Labels)) throw new Error("Missing cluster label");
        return row.Labels["io.x-k8s.kind.cluster"];
      });
    } else {
      const { stdout } = await execute(tools.kind, ["get", "clusters"], { env: tools.env, stdio: "pipe" });
      names = stdout.split(/\r?\n/).map(name => name.trim()).filter(Boolean);
    }
    if (!names.every((name): name is string => typeof name === "string" && /^[a-z0-9][a-z0-9-]*$/.test(name))) throw new Error("Invalid cluster inventory");
    return [...new Set(names)];
  } catch {
    throw new KindInventoryError(`Kind cluster discovery failed; absence was not established. ${podman
      ? "Check the selected runtime connection with podman system connection list and podman ps -a --format json"
      : "Check Docker availability and kind get clusters"} then retry. No cluster resources were changed.`);
  }
}
