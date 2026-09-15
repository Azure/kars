// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { at, PRIVATE_PREFIX, record, reviewed } from "./private-activation.js";
import { privateCommandFailure } from "./private-activation-command-diagnostics.js";

export type RootGuardExecute = (file: string, args: readonly string[], options: {
  stdio: "pipe"; timeout?: number;
}) => Promise<{ stdout: string }>;

export class PrivateRootUpgradeBlocked extends Error {
  constructor(readonly reason: "protected" | "unavailable", diagnostic?: string) {
    super(reason === "protected"
      ? "KARS_PRIVATE_ROOT_UPGRADE_BLOCKED: Controller changes are not supported while private qualification or retirement evidence exists. "
        + "Keep the root template and proofs unchanged. Original credentials grant preview/apply recovery remains available; "
        + "completing recovery does not authorize a template upgrade."
      : `KARS_PRIVATE_ROOT_CHECK_UNAVAILABLE: Cannot prove that controller changes are safe; no root-changing operation was authorized.${diagnostic ? ` ${diagnostic}` : ""}`);
    this.name = "PrivateRootUpgradeBlocked";
  }
}

function protectedMetadata(value: unknown): boolean {
  const metadata = record(value);
  return ["annotations", "labels"].some(field => {
    if (metadata[field] === undefined) return false;
    const values = record(metadata[field]);
    if (Object.values(values).some(item => typeof item !== "string")) throw new PrivateRootUpgradeBlocked("unavailable");
    // This is a refusal test, not a proof parser: malformed, unknown and
    // incomplete private receipts must never become an unqualified fallback.
    return Object.keys(values).some(key => key.startsWith(PRIVATE_PREFIX));
  });
}

function mutableReference(image: string): string {
  if (!image || /\s/.test(image)) throw new PrivateRootUpgradeBlocked("unavailable");
  const slash = image.indexOf("/");
  if (slash >= 0) image = image.slice(0, slash).toLowerCase().replace(/:443$/, "") + image.slice(slash);
  return image.lastIndexOf(":") > image.lastIndexOf("/") ? image : `${image}:latest`;
}

async function optionalRead(execute: RootGuardExecute, kind: string, name: string, namespace?: string) {
  const args = ["get", kind, name, ...(namespace ? ["-n", namespace] : []), "--ignore-not-found", "-o", "json"];
  let stdout: string;
  try {
    stdout = (await execute("kubectl", args, { stdio: "pipe", timeout: 15000 })).stdout;
  } catch (error) {
    throw new PrivateRootUpgradeBlocked("unavailable", privateCommandFailure(error, args, "Review").message);
  }
  if (!stdout.trim()) return undefined;
  const object = record(JSON.parse(stdout));
  if (reviewed(object).name !== name || (namespace && at(object, "metadata", "namespace") !== namespace)) {
    throw new PrivateRootUpgradeBlocked("unavailable");
  }
  return object;
}

/** Only successful API NotFound projections prove absence. No mutations,
 * admission dry-runs, grant edits or recovery exceptions occur in this guard. */
export async function assertControllerMutationAllowed(
  execute: RootGuardExecute, namespace = "kars-system",
  publication?: readonly string[],
): Promise<void> {
  try {
    const ns = await optionalRead(execute, "namespace", namespace);
    if (!ns) return;
    let protectedRoot = protectedMetadata(ns.metadata);
    if (protectedRoot && publication === undefined) throw new PrivateRootUpgradeBlocked("protected");
    const deployment = await optionalRead(execute, "deployment", "kars-controller", namespace);
    if (!deployment) {
      if (protectedRoot) throw new PrivateRootUpgradeBlocked("protected");
      return;
    }
    const template = record(at(deployment, "spec", "template"));
    protectedRoot ||= protectedMetadata(deployment.metadata) || protectedMetadata(template.metadata ?? {});
    if (!protectedRoot) return;
    if (publication === undefined) throw new PrivateRootUpgradeBlocked("protected");
    const targets = new Set(publication.map(mutableReference));
    for (const kind of ["containers", "initContainers"]) {
      const containers = at(template, "spec", kind) ?? [];
      if (!Array.isArray(containers) || (kind === "containers" && !containers.length)) throw new PrivateRootUpgradeBlocked("unavailable");
      for (const container of containers) {
        const image = at(container, "image");
        if (typeof image !== "string" || !image) throw new PrivateRootUpgradeBlocked("unavailable");
        if (image.includes("@")) {
          if (!/^[^@\s]+@sha256:[a-f0-9]{64}$/.test(image)) throw new PrivateRootUpgradeBlocked("unavailable");
          continue;
        }
        if (targets.has(mutableReference(image))) throw new PrivateRootUpgradeBlocked("protected");
      }
    }
  } catch (error) {
    if (error instanceof PrivateRootUpgradeBlocked) throw error;
    throw new PrivateRootUpgradeBlocked("unavailable");
  }
}

export async function assertRenderedControllersMutable(
  execute: RootGuardExecute, documents: readonly unknown[], namespace: string,
): Promise<void> {
  const namespaces = new Set<string>();
  for (const document of documents) {
    if (at(document, "kind") !== "Deployment" || at(document, "metadata", "name") !== "kars-controller") continue;
    const target = at(document, "metadata", "namespace") ?? namespace;
    if (typeof target !== "string" || !target) throw new PrivateRootUpgradeBlocked("unavailable");
    namespaces.add(target);
  }
  for (const target of namespaces) await assertControllerMutationAllowed(execute, target);
}
