// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { Execute } from "./deployment-target.js";
import { CLAIM, namespaceClaimed, type OwnershipObject } from "./namespace-ownership.js";
import {
  SOURCE, decode, get, identity, parse, run, targetName, validateSource, validateValues,
  type Metadata, type Sandbox, type Secret, type SourceRef,
} from "./credential-source-io.js";

export { FLAG_ENV, SOURCE_KEYS, removedKeys, updatesFromFlags } from "./credential-source-io.js";
export type { SourceRef } from "./credential-source-io.js";

export interface PreparedSource {
  reference: SourceRef;
  sandbox?: Sandbox;
}

function mergeValues(existing: Record<string, string>, updates: Record<string, string>, remove: string[]): Record<string, string> {
  const merged = { ...existing, ...updates };
  for (const key of remove) delete merged[key];
  return merged;
}

async function legacyValues(execute: Execute, sandbox: Sandbox): Promise<Record<string, string>> {
  const name = sandbox.metadata.name;
  const target = `kars-${name}`;
  const namespace = await get<OwnershipObject & { metadata: Metadata }>(execute, "namespace", target);
  if (!namespace) {
    if (sandbox.metadata.annotations?.["kars.azure.com/namespace-uid"]) {
      throw new Error("Bound runtime namespace is missing; refusing an incomplete credential migration");
    }
    return {};
  }
  if (!namespaceClaimed(namespace, sandbox as unknown as OwnershipObject)) {
    throw new Error("Credential migration requires namespace claim v1; finish namespace upgrade/adoption first");
  }
  return decode(await get<Secret>(execute, "secret", `${name}-credentials`, target));
}

async function writeSource(
  execute: Execute, name: string, workspace: string, values: Record<string, string>, existing?: Secret, sandbox?: Sandbox,
): Promise<SourceRef> {
  validateValues(values);
  const sourceName = targetName(name, workspace);
  const data: Record<string, string | null> = Object.create(null);
  for (const [key, value] of Object.entries(values)) data[key] = Buffer.from(value).toString("base64");
  let text: string;
  if (existing) {
    for (const key of Object.keys(existing.data ?? {})) if (!(key in values)) data[key] = null;
    const { uid, resourceVersion } = existing.metadata;
    text = await run(execute, "Update credential source", ["patch", "secret", sourceName,
      "-n", workspace, "--type=merge", "--patch-file=/dev/stdin", "-o", "json"], {
      metadata: { uid, resourceVersion }, data,
    });
  } else {
    text = await run(execute, "Create credential source", ["create", "-f", "-", "-o", "json"], {
      apiVersion: "v1", kind: "Secret", type: "Opaque",
      metadata: {
        name: sourceName, namespace: workspace,
        annotations: {
          [SOURCE.purpose]: "agent-source-v1", [SOURCE.target]: name,
          [SOURCE.workspace]: workspace, [SOURCE.intent]: "explicit-reference-v1",
        },
      }, data,
    });
  }
  const result = parse<Secret>(text);
  identity(result.metadata);
  if (result.metadata.name !== sourceName || result.metadata.namespace !== workspace
      || (existing && result.metadata.uid !== existing.metadata.uid)) {
    throw new Error("Credential source identity changed during write");
  }
  validateSource(result, name, workspace, sandbox);
  validateValues(decode(result));
  return { name: sourceName, uid: result.metadata.uid };
}

export async function prepareCredentialSource(
  execute: Execute, name: string, workspace: string, updates: Record<string, string>,
  remove: string[] = [], useCurrentIncarnation = true,
): Promise<PreparedSource> {
  const sourceName = targetName(name, workspace);
  const sandbox = await get<Sandbox>(execute, "karssandbox", name, workspace);
  const reference = sandbox?.spec.credentialsRef;
  if (reference && (reference.name !== sourceName || !reference.uid)) {
    throw new Error("Sandbox credential reference is invalid; explicitly disable it before repair");
  }
  const source = await get<Secret>(execute, "secret", sourceName, workspace);
  if (source) validateSource(source, name, workspace, sandbox);
  if (reference && !useCurrentIncarnation && source?.metadata.uid !== reference.uid) {
    throw new Error("Pinned credential source is missing/replaced; --use-source explicitly selects a new compatible incarnation");
  }
  // Source mode is an entire collection. Migration is one-time and read-only
  // against the existing target; active sources never fall back to legacy data.
  const legacy = sandbox && !reference ? await legacyValues(execute, sandbox) : {};
  const values = mergeValues({ ...legacy, ...decode(source) }, updates, remove);
  const selected = await writeSource(execute, name, workspace, values, source, sandbox);
  return { reference: selected, sandbox };
}

export async function verifyCredentialReference(
  execute: Execute, name: string, workspace: string, reference: SourceRef,
): Promise<Sandbox> {
  const sandbox = await get<Sandbox>(execute, "karssandbox", name, workspace);
  if (!sandbox || sandbox.spec.credentialsRef?.name !== reference.name
      || sandbox.spec.credentialsRef.uid !== reference.uid) {
    throw new Error("Cluster did not retain the pinned credential reference; upgrade the CRD/controller before using source mode");
  }
  return sandbox;
}

export async function applySourceSandbox(
  execute: Execute, manifest: Record<string, unknown>, prepared: PreparedSource,
): Promise<void> {
  const metadata = manifest.metadata as { name: string; namespace: string };
  const spec = { ...(manifest.spec as object), credentialsRef: prepared.reference };
  let written: Sandbox;
  if (prepared.sandbox) {
    const { uid, resourceVersion } = prepared.sandbox.metadata;
    written = parse<Sandbox>(await run(execute, "Bind existing Sandbox credentials", [
      "patch", "karssandbox", metadata.name, "-n", metadata.namespace,
      "--type=merge", "--patch-file=/dev/stdin", "-o", "json",
    ], { metadata: { uid, resourceVersion }, spec }));
    if (written.metadata.uid !== uid) throw new Error("Sandbox identity changed while binding credentials");
  } else {
    // CREATE, not apply: a racing Sandbox of the same name is not ours to bind.
    written = parse<Sandbox>(await run(execute, "Create source-bound Sandbox", ["create", "-f", "-", "-o", "json"], {
      ...manifest, spec,
    }));
  }
  identity(written.metadata);
  const confirmed = await verifyCredentialReference(execute, metadata.name, metadata.namespace, prepared.reference);
  if (confirmed.metadata.uid !== written.metadata.uid) throw new Error("Sandbox was recreated after source binding");
}

export async function waitForCredentialSource(
  execute: Execute, name: string, workspace: string, reference: SourceRef,
  options: { attempts?: number; delayMs?: number } = {},
): Promise<void> {
  const attempts = options.attempts ?? 30;
  for (let attempt = 0; attempt < attempts; attempt++) {
    const sandbox = await verifyCredentialReference(execute, name, workspace, reference);
    const text = await run(execute, "Read source reconciliation version", [
      "get", "secret", reference.name, "-n", workspace, "-o", "jsonpath={.metadata}",
    ]);
    const metadata = parse<Metadata>(text);
    identity(metadata);
    if (metadata.uid !== reference.uid) throw new Error("Credential source was replaced while awaiting reconciliation");
    const conditions = sandbox.status?.conditions ?? [];
    const ready = conditions.find(condition => condition.type === "CredentialsReady" && condition.status === "True");
    if (ready) {
      const observed = parse<{ sourceUid?: string; sourceVersion?: string }>(ready.message);
      if (observed.sourceUid === reference.uid && observed.sourceVersion === metadata.resourceVersion) return;
    }
    // An earlier failure condition can precede this source revision. Allow the
    // controller to process a repair rather than treating stale status as final.
    if (attempt + 1 < attempts) await new Promise(resolve => setTimeout(resolve, options.delayMs ?? 3000));
  }
  throw new Error("Controller did not confirm credential source reconciliation; check compatible controller/CRD versions and Sandbox conditions");
}

export async function updateCredentialSource(
  execute: Execute, name: string, workspace: string,
  options: { updates: Record<string, string>; remove: string[]; useSource?: boolean; disableSource?: boolean; restart?: boolean },
): Promise<{ kind: "source"; reference?: SourceRef; staged?: boolean } | undefined> {
  targetName(name, workspace);
  const sandbox = await get<Sandbox>(execute, "karssandbox", name, workspace);
  if (!sandbox?.spec.credentialsRef && !options.useSource && !options.disableSource) {
    const namespace = await get<OwnershipObject & { metadata: Metadata }>(execute, "namespace", `kars-${name}`);
    if (namespace?.metadata.annotations?.[CLAIM.version] !== undefined) {
      if (!sandbox || !namespaceClaimed(namespace, sandbox as unknown as OwnershipObject)) {
        throw new Error("Runtime namespace belongs to another or missing Sandbox; select its workspace with --namespace");
      }
    }
    return undefined;
  }
  if (options.restart === false) throw new Error("Source mode always refreshes runtime credentials; --no-restart is only supported for direct credentials");
  if (options.disableSource) {
    if (!sandbox) throw new Error("Sandbox does not exist");
    if (options.useSource || options.remove.length || Object.keys(options.updates).length) {
      throw new Error("--disable-source cannot be combined with credential changes");
    }
    if (sandbox.spec.credentialsRef) {
      const { uid, resourceVersion } = sandbox.metadata;
      const result = parse<Sandbox>(await run(execute, "Disable credential source", [
        "patch", "karssandbox", name, "-n", workspace, "--type=merge", "--patch-file=/dev/stdin", "-o", "json",
      ], { metadata: { uid, resourceVersion }, spec: { credentialsRef: null } }));
      if (result.spec.credentialsRef) throw new Error("Controller API did not remove the source reference");
    }
    return { kind: "source" };
  }
  const prepared = await prepareCredentialSource(execute, name, workspace, options.updates, options.remove, options.useSource === true);
  if (prepared.sandbox && (prepared.sandbox.spec.credentialsRef?.uid !== prepared.reference.uid
      || prepared.sandbox.spec.credentialsRef?.name !== prepared.reference.name)) {
    await applySourceSandbox(execute, {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox",
      metadata: { name, namespace: workspace }, spec: prepared.sandbox.spec,
    }, prepared);
  }
  if (prepared.sandbox) await waitForCredentialSource(execute, name, workspace, prepared.reference);
  return { kind: "source", reference: prepared.reference, staged: !prepared.sandbox };
}

/** Preserve unrelated direct keys; explicit nulls remove keys across managers
 * without deleting/recreating customer Secrets. */
export async function updateDirectCredentials(
  execute: Execute, name: string, updates: Record<string, string>, remove: string[],
): Promise<void> {
  targetName(name, "kars-system");
  const namespace = `kars-${name}`;
  const secretName = `${name}-credentials`;
  const existing = await get<Secret>(execute, "secret", secretName, namespace);
  const data: Record<string, string | null> = Object.create(null);
  for (const [key, value] of Object.entries(updates)) data[key] = Buffer.from(value).toString("base64");
  for (const key of remove) data[key] = null;
  if (existing) {
    await run(execute, "Update direct credentials", [
      "patch", "secret", secretName, "-n", namespace, "--type=merge", "--patch-file=/dev/stdin",
    ], { metadata: { uid: existing.metadata.uid, resourceVersion: existing.metadata.resourceVersion }, data });
  } else if (Object.keys(updates).some(key => !remove.includes(key))) {
    for (const key of remove) delete data[key];
    await run(execute, "Create direct credentials", ["create", "-f", "-"], {
      apiVersion: "v1", kind: "Secret", type: "Opaque", metadata: { name: secretName, namespace }, data,
    });
  }
}
