// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { BetaExecute } from "../dev/kind-beta.js";
import { readReleaseValues } from "../../lib/mesh-release.js";
import { SetupError } from "./types.js";

export interface SetupCoreInventory {
  context: string;
  namespaceUid?: string;
  status: "absent" | "unmanaged" | "installed";
  release?: string;
  chart?: string;
  controllerUid?: string;
  provider?: string;
  model?: string;
}

function identifier(value: unknown): string | undefined {
  return typeof value === "string" && /^[a-zA-Z0-9][a-zA-Z0-9._:/+-]{0,255}$/.test(value) ? value : undefined;
}

/** Discover public defaults only; existing credentials and authority are never adopted here. */
export async function inspectSetupCore(context: string, execute: BetaExecute): Promise<SetupCoreInventory> {
  const scoped: BetaExecute = (tool, args, options) => execute(tool,
    tool === "helm" ? ["--kube-context", context, ...args] : ["--context", context, "--request-timeout=15s", ...args], options);
  const get = async (kind: string, name: string, namespaced = true) => {
    const { stdout } = await scoped("kubectl", ["get", kind, name, ...(namespaced ? ["-n", "kars-system"] : []), "--ignore-not-found=true", "-o", "json"], { stdio: "pipe" });
    return stdout.trim() ? JSON.parse(stdout) : undefined;
  };
  const namespace = await get("namespace", "kars-system", false);
  if (!namespace) return { context, status: "absent" };
  if (!identifier(namespace.metadata?.uid) || namespace.metadata.deletionTimestamp) throw new SetupError("Core namespace identity is unavailable or terminating; no installation or adoption is allowed");
  const inventory: SetupCoreInventory = { context, namespaceUid: namespace.metadata.uid, status: "unmanaged" };
  const controller = await get("deployment", "kars-controller");
  const metadata = controller?.metadata;
  if (!identifier(metadata?.uid) || !identifier(metadata?.resourceVersion) || metadata.deletionTimestamp
    || metadata.labels?.["app.kubernetes.io/managed-by"] !== "Helm"
    || metadata.annotations?.["meta.helm.sh/release-name"] !== "kars"
    || metadata.annotations?.["meta.helm.sh/release-namespace"] !== "kars-system") return inventory;
  const releases = JSON.parse((await scoped("helm", ["list", "-n", "kars-system", "--all", "--filter", "^kars$", "-o", "json"], { stdio: "pipe" })).stdout);
  if (!Array.isArray(releases) || releases.length !== 1 || releases[0].name !== "kars" || releases[0].namespace !== "kars-system"
    || releases[0].status !== "deployed" || !/^kars-\d/.test(identifier(releases[0].chart) ?? "")) {
    throw new SetupError("Core release is missing, ambiguous, or not deployed; preserve its resources and inspect it before continuing");
  }
  const values = await readReleaseValues(scoped);
  const defaults = (values.controller as { inferenceDefaults?: { provider?: unknown; model?: unknown } } | undefined)?.inferenceDefaults;
  const [currentNamespace, currentController] = await Promise.all([get("namespace", "kars-system", false), get("deployment", "kars-controller")]);
  if (currentNamespace?.metadata?.uid !== namespace.metadata.uid || currentNamespace.metadata.deletionTimestamp
    || currentController?.metadata?.uid !== metadata.uid || currentController.metadata.deletionTimestamp
    || currentController.metadata.resourceVersion !== metadata.resourceVersion) {
    throw new SetupError("Core changed during discovery; retry inspection without modifying existing resources");
  }
  return { ...inventory, status: "installed", release: "kars", chart: releases[0].chart, controllerUid: metadata.uid,
    provider: identifier(defaults?.provider), model: identifier(defaults?.model) };
}
