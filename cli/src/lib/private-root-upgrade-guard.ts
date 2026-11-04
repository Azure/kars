// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { at, canonical, PRIVATE_PREFIX, record, reviewed } from "./private-activation.js";
import { privateCommandFailure } from "./private-activation-command-diagnostics.js";

export type RootGuardExecute = (file: string, args: readonly string[], options: {
  stdio: "pipe"; timeout?: number; input?: string;
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
  previousDocuments: readonly unknown[] = [],
  fencedHelmNoop = false,
): Promise<void> {
  const controllers = new Map<string, unknown[]>();
  for (const document of [...documents, ...previousDocuments]) {
    if (at(document, "kind") !== "Deployment" || at(document, "metadata", "name") !== "kars-controller") continue;
    const target = at(document, "metadata", "namespace") ?? namespace;
    if (typeof target !== "string" || !target) throw new PrivateRootUpgradeBlocked("unavailable");
    if (!controllers.has(target)) controllers.set(target, []);
    if (documents.includes(document)) controllers.get(target)!.push(document);
  }
  for (const [target, desired] of controllers) {
    try {
      await assertControllerMutationAllowed(execute, target);
    } catch (error) {
      if (!(error instanceof PrivateRootUpgradeBlocked) || error.reason !== "protected") throw error;
      // An absent target would delete the root. Historical manifests discover
      // deletions but are not an upgrade's desired controller configuration.
      if (!fencedHelmNoop || desired.length !== 1) throw error;
      await assertFencedControllerUnchanged(execute, desired[0], target, [...documents, ...previousDocuments]);
    }
  }
}

async function assertFencedControllerUnchanged(
  execute: RootGuardExecute, desired: unknown, namespace: string, releaseDocuments: readonly unknown[],
): Promise<void> {
  try {
    if (["uid", "resourceVersion"].some(key => typeof at(desired, "metadata", key) !== "string"
      || !at(desired, "metadata", key))) throw new PrivateRootUpgradeBlocked("protected");
    const identity = reviewed(desired);
    if (at(desired, "apiVersion") !== "apps/v1"
      || at(desired, "metadata", "namespace") !== namespace) throw new PrivateRootUpgradeBlocked("protected");
    const beforeNamespace = await optionalRead(execute, "namespace", namespace);
    const live = await optionalRead(execute, "deployment", "kars-controller", namespace);
    if (!beforeNamespace || !live || canonical(reviewed(live)) !== canonical(identity)) {
      throw new PrivateRootUpgradeBlocked("protected");
    }
    const privateFields = record(at(beforeNamespace, "metadata", "annotations") ?? {});
    const budget = ["budget-namespace", "budget-tls-name", "budget-tls-version"]
      .map(key => privateFields[`${PRIVATE_PREFIX}${key}`]);
    if (budget.some(value => value !== undefined)) {
      if (budget.some(value => typeof value !== "string" || !value)
        || releaseDocuments.some(document => at(document, "kind") === "Secret"
          && at(document, "metadata", "name") === budget[1]
          && (at(document, "metadata", "namespace") ?? namespace) === budget[0])) {
        // Even identical data can receive a new revision when Helm takes SSA ownership.
        throw new PrivateRootUpgradeBlocked("protected");
      }
    }
    const annotations = record(at(live, "metadata", "annotations") ?? {});
    const release = annotations["meta.helm.sh/release-name"];
    if (typeof release !== "string" || !release
      || annotations["meta.helm.sh/release-namespace"] !== namespace
      || at(live, "metadata", "labels", "app.kubernetes.io/managed-by") !== "Helm") {
      throw new PrivateRootUpgradeBlocked("protected");
    }
    const template = record(at(live, "spec", "template"));
    for (const kind of ["containers", "initContainers"]) {
      const containers = at(template, "spec", kind) ?? [];
      if (!Array.isArray(containers) || (kind === "containers" && !containers.length)
        || containers.some(container => typeof at(container, "image") !== "string"
          || !/^[^@\s]+@sha256:[a-f0-9]{64}$/.test(String(at(container, "image"))))) {
        throw new PrivateRootUpgradeBlocked("protected");
      }
    }
    const request = structuredClone(record(desired));
    const metadata = record(request.metadata);
    for (const [key, value] of Object.entries({
      "meta.helm.sh/release-name": release, "meta.helm.sh/release-namespace": namespace,
    })) {
      const requested = at(metadata, "annotations", key);
      if (requested !== undefined && requested !== value) throw new PrivateRootUpgradeBlocked("protected");
    }
    // Helm adds its ownership metadata after template rendering.
    metadata.labels = { ...record(metadata.labels ?? {}), "app.kubernetes.io/managed-by": "Helm" };
    metadata.annotations = { ...record(metadata.annotations ?? {}),
      "meta.helm.sh/release-name": release, "meta.helm.sh/release-namespace": namespace };
    const preview = JSON.parse((await execute("kubectl", ["apply", "--server-side", "--field-manager=helm",
      "--dry-run=server", "-f", "-", "-o", "json"],
    { stdio: "pipe", input: JSON.stringify(request), timeout: 15000 })).stdout);
    const projection = (object: unknown) => ({
      identity: reviewed(object), apiVersion: at(object, "apiVersion"), kind: at(object, "kind"),
      namespace: at(object, "metadata", "namespace"),
      labels: at(object, "metadata", "labels") ?? {}, annotations: at(object, "metadata", "annotations") ?? {},
      ownerReferences: at(object, "metadata", "ownerReferences") ?? [],
      finalizers: at(object, "metadata", "finalizers") ?? [],
      spec: at(object, "spec"),
    });
    if (canonical(projection(preview)) !== canonical(projection(live))
      || canonical(await optionalRead(execute, "namespace", namespace)) !== canonical(beforeNamespace)
      || canonical(await optionalRead(execute, "deployment", "kars-controller", namespace)) !== canonical(live)) {
      throw new PrivateRootUpgradeBlocked("protected");
    }
  } catch (error) {
    if (error instanceof PrivateRootUpgradeBlocked) throw error;
    // Neither failed admission nor malformed evidence authorizes a real write.
    throw new PrivateRootUpgradeBlocked("unavailable");
  }
}
