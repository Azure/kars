// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Chunked mesh transport — extracted from plugin.ts in S15.f.3.
//
// Provides the wire-level large-payload chunking protocol used by every
// mesh-borne large blob (mesh_send tool, mesh_transfer_file tool, handoff
// blob transfers, sub-agent workspace harvest). The Signal Protocol
// session and AGT identity remain in plugin.ts; this module is purely
// the transport shim that splits / reassembles JSON.
//
// Protocol (callers see send/receive as a single message):
//   1. Sender calls meshSend(client, target, message)
//   2. ≤ MESH_CHUNK_THRESHOLD (512 KB): sent as-is (fast path)
//   3. > threshold: sends mesh:transfer_manifest + N mesh:transfer_chunk
//   4. Receiver runs each inbound message through meshHandleTransportMessage:
//      - returns undefined → not transport, app layer handles it
//      - returns null      → absorbed (manifest, partial chunks, or rejects)
//      - returns object    → reassembled application-layer message
//
// Per-chunk + manifest SHA-256 hashes are integrity-checked at the receiver.

import { pushSigningCounter } from "./router-client.js";

export const MESH_CHUNK_THRESHOLD = 512 * 1024;
export const MESH_CHUNK_SIZE = 512 * 1024;
export const MESH_MAX_CHUNKS = 80;
export const MESH_TRANSFER_TTL = 120_000;

export interface PendingMeshTransfer {
  from_amid: string;
  from_agent: string;
  transfer_id: string;
  total_chunks: number;
  total_bytes: number;
  chunk_hashes: string[];
  manifest_hash: string;
  metadata: Record<string, unknown>;
  chunks: Map<number, string>;
  received_at: number;
}

export const pendingTransfers = new Map<string, PendingMeshTransfer>();

const cleanupTimer = setInterval(() => {
  const now = Date.now();
  for (const [key, t] of pendingTransfers) {
    if (now - t.received_at > MESH_TRANSFER_TTL) pendingTransfers.delete(key);
  }
}, 30_000);
cleanupTimer.unref();

interface MeshIdentity {
  amid: string;
  signB64(payload: Uint8Array): Promise<string>;
}
type MeshLogger = { info: (m: string) => void; warn: (m: string) => void };

export async function meshSendWithIdentity(
  client: { send: (amid: string, msg: unknown) => Promise<void> },
  targetAmid: string,
  message: Record<string, unknown>,
  identity: MeshIdentity | null,
  _log?: MeshLogger,
): Promise<string | undefined> {
  // Ed25519 per-message signing — attach signature and sender AMID
  if (identity && !message.__signed) {
    const payload = JSON.stringify(message);
    try {
      const encoder = new TextEncoder();
      const signature = await identity.signB64(encoder.encode(payload));
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (message as any).__signature = signature;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (message as any).__sender_amid = identity.amid;
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (message as any).__signed_at = Math.floor(Date.now() / 1000);
      pushSigningCounter("signed");
    } catch {
      // Sign failed — send unsigned (fail-open for availability)
    }
  }

  const json = JSON.stringify(message);

  if (json.length <= MESH_CHUNK_THRESHOLD) {
    await client.send(targetAmid, message);
    return undefined;
  }

  // Large payload — chunked transfer
  const totalChunks = Math.ceil(json.length / MESH_CHUNK_SIZE);
  if (totalChunks > MESH_MAX_CHUNKS) {
    throw new Error(
      `Payload too large for mesh transfer: ${(json.length / 1024 / 1024).toFixed(1)} MB ` +
      `(${totalChunks} chunks exceeds max ${MESH_MAX_CHUNKS})`
    );
  }

  const { createHash } = await import("node:crypto");
  const transferId = crypto.randomUUID();
  const fromAgent = String(message.from_agent || process.env.SANDBOX_NAME || "unknown");

  const chunkHashes: string[] = [];
  for (let i = 0; i < totalChunks; i++) {
    const chunk = json.slice(i * MESH_CHUNK_SIZE, (i + 1) * MESH_CHUNK_SIZE);
    chunkHashes.push(createHash("sha256").update(chunk).digest("hex"));
  }
  const manifestHash = createHash("sha256").update(chunkHashes.join(":")).digest("hex");

  _log?.info(
    `Mesh chunked send: ${(json.length / 1024).toFixed(0)} KB → ${totalChunks} chunks ` +
    `(transfer ${transferId.slice(0, 8)})`
  );

  await client.send(targetAmid, {
    type: "mesh:transfer_manifest",
    transfer_id: transferId,
    original_type: message.type || "message",
    total_chunks: totalChunks,
    total_bytes: json.length,
    chunk_hashes: chunkHashes,
    manifest_hash: manifestHash,
    from_agent: fromAgent,
    timestamp: new Date().toISOString(),
  });

  for (let i = 0; i < totalChunks; i++) {
    const chunkData = json.slice(i * MESH_CHUNK_SIZE, (i + 1) * MESH_CHUNK_SIZE);
    await client.send(targetAmid, {
      type: "mesh:transfer_chunk",
      transfer_id: transferId,
      chunk_index: i,
      total_chunks: totalChunks,
      data: chunkData,
      hash: chunkHashes[i],
      from_agent: fromAgent,
    });
  }

  _log?.info(`Mesh chunked send complete: ${totalChunks} chunks (transfer ${transferId.slice(0, 8)})`);
  return transferId;
}

/**
 * Handle an inbound message that may or may not be part of the chunked
 * transport layer. Returns:
 *   - undefined → not a transport message; app layer should handle.
 *   - null      → absorbed (manifest stored, partial chunk, or reject).
 *   - object    → fully reassembled application-layer message.
 */
export async function meshHandleTransportMessage(
  fromAmid: string,
  fromAgent: string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  message: any,
  _log?: MeshLogger,
): Promise<Record<string, unknown> | null | undefined> {
  const msgType = message?.type;
  if (msgType !== "mesh:transfer_manifest" && msgType !== "mesh:transfer_chunk") {
    return undefined;
  }

  const transferId = message.transfer_id;
  if (!transferId) return null;

  const key = `${fromAmid}:${transferId}`;

  if (msgType === "mesh:transfer_manifest") {
    pendingTransfers.set(key, {
      from_amid: fromAmid,
      from_agent: fromAgent,
      transfer_id: transferId,
      total_chunks: message.total_chunks,
      total_bytes: message.total_bytes,
      chunk_hashes: message.chunk_hashes || [],
      manifest_hash: message.manifest_hash || "",
      metadata: { original_type: message.original_type },
      chunks: new Map(),
      received_at: Date.now(),
    });
    _log?.info(
      `Mesh transfer manifest: ${message.total_chunks} chunks, ` +
      `${(message.total_bytes / 1024).toFixed(0)} KB (transfer ${transferId.slice(0, 8)})`
    );
    return null;
  }

  // mesh:transfer_chunk
  const transfer = pendingTransfers.get(key);
  if (!transfer) {
    for (const [, t] of pendingTransfers) {
      if (t.transfer_id === transferId && t.from_amid === fromAmid) {
        t.chunks.set(message.chunk_index, message.data);
        return null;
      }
    }
    _log?.warn(`Mesh chunk for unknown transfer ${transferId.slice(0, 8)} — dropped`);
    return null;
  }

  if (message.hash && transfer.chunk_hashes[message.chunk_index]) {
    const { createHash } = await import("node:crypto");
    const computed = createHash("sha256").update(message.data).digest("hex");
    if (computed !== message.hash) {
      _log?.warn(
        `Mesh chunk ${message.chunk_index} hash mismatch (transfer ${transferId.slice(0, 8)}) — rejected`
      );
      return null;
    }
  }

  transfer.chunks.set(message.chunk_index, message.data);

  if (transfer.chunks.size < transfer.total_chunks) {
    if (transfer.chunks.size % 10 === 0) {
      _log?.info(
        `Mesh transfer ${transferId.slice(0, 8)}: ${transfer.chunks.size}/${transfer.total_chunks} chunks`
      );
    }
    return null;
  }

  _log?.info(
    `Mesh transfer ${transferId.slice(0, 8)}: all ${transfer.total_chunks} chunks received — reassembling`
  );

  const { createHash: mHash } = await import("node:crypto");
  const actualHashes: string[] = [];
  for (let i = 0; i < transfer.total_chunks; i++) {
    actualHashes.push(
      mHash("sha256").update(transfer.chunks.get(i) || "").digest("hex")
    );
  }
  const actualManifestHash = mHash("sha256").update(actualHashes.join(":")).digest("hex");
  if (transfer.manifest_hash && actualManifestHash !== transfer.manifest_hash) {
    _log?.warn(
      `Mesh transfer ${transferId.slice(0, 8)}: manifest hash mismatch — ` +
      `data may be corrupted (expected ${transfer.manifest_hash.slice(0, 12)}, ` +
      `got ${actualManifestHash.slice(0, 12)})`
    );
  }

  const parts: string[] = [];
  for (let i = 0; i < transfer.total_chunks; i++) {
    parts.push(transfer.chunks.get(i) || "");
  }
  const reassembledJson = parts.join("");
  pendingTransfers.delete(key);

  let reassembled: Record<string, unknown>;
  try {
    reassembled = JSON.parse(reassembledJson);
  } catch {
    _log?.warn(`Mesh transfer ${transferId.slice(0, 8)}: reassembled JSON parse failed`);
    return null;
  }

  _log?.info(
    `Mesh transfer ${transferId.slice(0, 8)}: reassembled ${(reassembledJson.length / 1024).toFixed(0)} KB ` +
    `(type: ${reassembled.type || transfer.metadata.original_type})`
  );

  return reassembled;
}
