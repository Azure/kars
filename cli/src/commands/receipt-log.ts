// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";

const HEAD = "kars-receipt-log";
const PREFIX = `${HEAD}-`;
const COMPONENT = "receipt-inclusion-log-segment";

export interface InclusionEntry {
  seq: number;
  receipt: string;
  payloadSha256: string;
  prevHash: string;
  entryHash: string;
}

/** Existing hash recipe, byte-identical to controller/src/kars_receipt_log.rs. */
export function inclusionEntryHash(
  seq: number,
  receipt: string,
  payloadSha256: string,
  prevHash: string,
): string {
  return createHash("sha256")
    .update(`${seq}|${receipt}|${payloadSha256}|${prevHash}`)
    .digest("hex");
}

export function verifyInclusionChain(chain: InclusionEntry[]): number | null {
  let prev = "genesis";
  for (let i = 0; i < chain.length; i++) {
    const e = chain[i];
    if (e.seq !== i) return i;
    if (e.prevHash !== prev) return e.seq;
    if (inclusionEntryHash(e.seq, e.receipt, e.payloadSha256, e.prevHash) !== e.entryHash) {
      return e.seq;
    }
    prev = e.entryHash;
  }
  return null;
}

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function entries(value: unknown): value is InclusionEntry[] {
  return Array.isArray(value) && value.every((entry: unknown) =>
    record(entry) && Number.isSafeInteger(entry.seq) && typeof entry.seq === "number" && entry.seq >= 0
    && typeof entry.receipt === "string" && typeof entry.payloadSha256 === "string"
    && typeof entry.prevHash === "string" && typeof entry.entryHash === "string");
}

/** Absence is distinct from an unreadable, truncated or corrupt log. */
export function parseInclusionSnapshot(snapshot: unknown, namespace: string): InclusionEntry[] | null {
  if (!record(snapshot) || !Array.isArray(snapshot.items) || !record(snapshot.metadata)
    || typeof snapshot.metadata.resourceVersion !== "string" || !snapshot.metadata.resourceVersion
    || (snapshot.metadata.continue !== undefined && snapshot.metadata.continue !== "")) {
    throw new Error("Receipt log snapshot is incomplete");
  }
  const segments = new Map<number, { raw: string; index?: unknown; previous?: unknown }>();
  for (const item of snapshot.items) {
    if (!record(item) || !record(item.metadata) || typeof item.metadata.name !== "string") {
      throw new Error("Receipt log snapshot has malformed ConfigMap metadata");
    }
    const meta = item.metadata;
    const name = item.metadata.name;
    const component = record(meta.labels) ? meta.labels["app.kubernetes.io/component"] : undefined;
    let index: number;
    if (name === HEAD) {
      if (component !== undefined && component !== "receipt-inclusion-log") {
        throw new Error("Receipt log head has a foreign component");
      }
      index = 0;
    } else if (name.startsWith(PREFIX)) {
      const suffix = name.slice(PREFIX.length);
      index = Number(suffix);
      if (!Number.isSafeInteger(index) || index <= 0 || suffix !== String(index).padStart(6, "0")
        || component !== COMPONENT) {
        throw new Error("Receipt log segment identity is invalid");
      }
    } else {
      if (component === COMPONENT) throw new Error("Receipt segment has an unexpected name");
      continue;
    }
    if (meta.namespace !== namespace || typeof meta.uid !== "string" || !meta.uid
      || typeof meta.resourceVersion !== "string" || !meta.resourceVersion
      || (meta.deletionTimestamp !== undefined && meta.deletionTimestamp !== null)
      || (meta.ownerReferences !== undefined
        && (!Array.isArray(meta.ownerReferences) || meta.ownerReferences.length !== 0))
      || !record(item.data) || typeof item.data["chain.json"] !== "string") {
      throw new Error("Receipt segment lacks current identity/data or has a foreign owner");
    }
    if (segments.has(index)) throw new Error("Receipt log has duplicate segment indexes");
    segments.set(index, {
      raw: item.data["chain.json"],
      index: item.data.segmentIndex,
      previous: item.data.previousRootHash,
    });
  }
  if (segments.size === 0) return null;
  const chain: InclusionEntry[] = [];
  for (let index = 0; index < segments.size; index++) {
    const segment = segments.get(index);
    if (!segment) throw new Error("Receipt log has a missing segment");
    let parsed: unknown;
    try {
      parsed = JSON.parse(segment.raw);
    } catch {
      throw new Error("Receipt segment JSON is invalid");
    }
    if (!entries(parsed)) throw new Error("Receipt segment entries are malformed");
    if (index > 0 && (parsed.length === 0 || segment.index !== String(index)
      || segment.previous !== (chain.at(-1)?.entryHash ?? "genesis"))) {
      throw new Error("Receipt segment index/previous-root binding is invalid");
    }
    for (const entry of parsed) chain.push(entry);
  }
  const broken = verifyInclusionChain(chain);
  if (broken !== null) throw new Error(`Receipt log chain is broken at seq ${broken}`);
  return chain;
}

export async function readInclusionChain(namespace: string): Promise<InclusionEntry[] | null> {
  if (!/^[a-z0-9](?:[-a-z0-9]{0,61}[a-z0-9])?$/.test(namespace)) {
    throw new Error("Receipt log namespace must be a Kubernetes namespace name");
  }
  const { execa } = await import("execa");
  // kubectl's ordinary JSON printer drops the collection resourceVersion.
  // --raw preserves one complete, versioned API snapshot without discovery.
  const command = await execa("kubectl", [
    "get", "--raw", `/api/v1/namespaces/${namespace}/configmaps`,
  ], { stdio: "pipe", reject: false });
  if (command.failed) throw new Error("Unable to read receipt log ConfigMaps; verify API access");
  let snapshot: unknown;
  try {
    snapshot = JSON.parse(command.stdout);
  } catch {
    throw new Error("Receipt log API response is not valid JSON");
  }
  return parseInclusionSnapshot(snapshot, namespace);
}
