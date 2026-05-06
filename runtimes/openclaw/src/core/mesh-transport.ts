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
//
// Recovery (added 2026-05 after the binary-transfer-fail-on-ratchet-desync
// production incident):
//   • Sender keeps each outbound transfer's chunks cached for
//     MESH_OUTBOUND_TTL so a receiver gap-request can be honored even after
//     the original send loop has returned.
//   • Receiver, after MESH_GAP_DETECT_MS without all chunks present,
//     emits mesh:transfer_chunk_request listing missing indices. Sender
//     responds by resending only those chunks (up to MESH_GAP_RETRIES).
//   • Receiver buffers orphan chunks (chunk arriving before its manifest)
//     for MESH_ORPHAN_BUFFER_MS rather than dropping them — covers the
//     SDK out-of-order delivery path.
//   • On successful reassembly, receiver emits mesh:transfer_complete so
//     the sender can free the outbound cache eagerly.

import { pushSigningCounter } from "./router-client.js";

export const MESH_CHUNK_THRESHOLD = 512 * 1024;
export const MESH_CHUNK_SIZE = 512 * 1024;
export const MESH_MAX_CHUNKS = 80;
export const MESH_TRANSFER_TTL = 120_000;

// Recovery tunables.
export const MESH_OUTBOUND_TTL = 90_000;        // sender cache lifetime
export const MESH_GAP_DETECT_MS = 8_000;        // receiver waits this long, then asks for missing chunks
export const MESH_GAP_RETRIES = 2;              // max receiver-initiated retransmit cycles
export const MESH_ORPHAN_BUFFER_MS = 15_000;    // hold orphan chunks this long awaiting their manifest

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
  /** When set, a setTimeout handle fires the gap-detection probe. */
  gap_timer?: NodeJS.Timeout;
  /** Number of times this receiver has already requested missing chunks. */
  gap_retries: number;
  /** True once mesh:transfer_complete has been emitted (idempotency). */
  ack_sent?: boolean;
}

export const pendingTransfers = new Map<string, PendingMeshTransfer>();

// Sender-side cache for retransmit on receiver gap-request.
interface OutboundTransfer {
  target_amid: string;
  total_chunks: number;
  /** Same hashes as sent in the manifest — included on retransmit for integrity. */
  chunk_hashes: string[];
  /** Pre-sliced chunk payloads (the JSON substring shipped on each chunk). */
  chunk_data: string[];
  from_agent: string;
  expires_at: number;
}
export const outboundTransfers = new Map<string, OutboundTransfer>();

// Orphan chunks: chunks that arrived before their manifest. Keyed by
// `${fromAmid}:${transferId}`. Held briefly so a delayed manifest can
// adopt them.
interface OrphanBuffer {
  chunks: Map<number, { data: string; hash?: string }>;
  first_seen: number;
}
const orphanChunks = new Map<string, OrphanBuffer>();

const cleanupTimer = setInterval(() => {
  const now = Date.now();
  for (const [key, t] of pendingTransfers) {
    if (now - t.received_at > MESH_TRANSFER_TTL) {
      if (t.gap_timer) clearTimeout(t.gap_timer);
      pendingTransfers.delete(key);
    }
  }
  for (const [key, t] of outboundTransfers) {
    if (now > t.expires_at) outboundTransfers.delete(key);
  }
  for (const [key, ob] of orphanChunks) {
    if (now - ob.first_seen > MESH_ORPHAN_BUFFER_MS) orphanChunks.delete(key);
  }
}, 30_000);
cleanupTimer.unref();

export interface MeshIdentity {
  amid: string;
  signB64(payload: Uint8Array): Promise<string>;
}
type MeshLogger = { info: (m: string) => void; warn: (m: string) => void };
type MeshSendClient = { send: (amid: string, msg: unknown) => Promise<void> };

export async function meshSendWithIdentity(
  client: MeshSendClient,
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
  const chunkData: string[] = [];
  for (let i = 0; i < totalChunks; i++) {
    const chunk = json.slice(i * MESH_CHUNK_SIZE, (i + 1) * MESH_CHUNK_SIZE);
    chunkData.push(chunk);
    chunkHashes.push(createHash("sha256").update(chunk).digest("hex"));
  }
  const manifestHash = createHash("sha256").update(chunkHashes.join(":")).digest("hex");

  // Cache for receiver-initiated retransmit. Inserted BEFORE the manifest
  // ships so a fast gap-request can be honored even mid-loop.
  outboundTransfers.set(transferId, {
    target_amid: targetAmid,
    total_chunks: totalChunks,
    chunk_hashes: chunkHashes,
    chunk_data: chunkData,
    from_agent: fromAgent,
    expires_at: Date.now() + MESH_OUTBOUND_TTL,
  });

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
    await client.send(targetAmid, {
      type: "mesh:transfer_chunk",
      transfer_id: transferId,
      chunk_index: i,
      total_chunks: totalChunks,
      data: chunkData[i],
      hash: chunkHashes[i],
      from_agent: fromAgent,
    });
  }

  _log?.info(`Mesh chunked send complete: ${totalChunks} chunks (transfer ${transferId.slice(0, 8)})`);
  return transferId;
}

/**
 * Send mesh:transfer_complete back to the original sender so it can free the
 * outbound retransmit cache eagerly.
 */
async function sendTransferComplete(
  client: MeshSendClient,
  fromAmid: string,
  transferId: string,
  fromAgent: string,
  log?: MeshLogger,
): Promise<void> {
  try {
    await client.send(fromAmid, {
      type: "mesh:transfer_complete",
      transfer_id: transferId,
      from_agent: process.env.SANDBOX_NAME || fromAgent,
      timestamp: new Date().toISOString(),
    });
  } catch (e: unknown) {
    // Non-fatal — the sender's cache will TTL out anyway.
    log?.warn(`mesh:transfer_complete send failed for ${transferId.slice(0, 8)}: ${(e as Error)?.message || e}`);
  }
}

/**
 * Schedule a gap-detection probe for an in-progress transfer. After
 * MESH_GAP_DETECT_MS without all chunks, send mesh:transfer_chunk_request
 * listing missing indices. Re-armed up to MESH_GAP_RETRIES times.
 */
function armGapTimer(
  client: MeshSendClient,
  key: string,
  log?: MeshLogger,
): void {
  const transfer = pendingTransfers.get(key);
  if (!transfer) return;
  if (transfer.gap_timer) clearTimeout(transfer.gap_timer);
  transfer.gap_timer = setTimeout(() => {
    void requestMissingChunks(client, key, log);
  }, MESH_GAP_DETECT_MS);
  transfer.gap_timer.unref?.();
}

async function requestMissingChunks(
  client: MeshSendClient,
  key: string,
  log?: MeshLogger,
): Promise<void> {
  const transfer = pendingTransfers.get(key);
  if (!transfer) return;
  if (transfer.chunks.size >= transfer.total_chunks) return; // raced — already done
  if (transfer.gap_retries >= MESH_GAP_RETRIES) {
    log?.warn(
      `Mesh transfer ${transfer.transfer_id.slice(0, 8)}: gap retries exhausted ` +
      `(${transfer.chunks.size}/${transfer.total_chunks} chunks) — abandoning`
    );
    return;
  }
  transfer.gap_retries += 1;

  const missing: number[] = [];
  for (let i = 0; i < transfer.total_chunks; i++) {
    if (!transfer.chunks.has(i)) missing.push(i);
  }
  log?.info(
    `Mesh transfer ${transfer.transfer_id.slice(0, 8)}: requesting ${missing.length} ` +
    `missing chunk(s) (retry ${transfer.gap_retries}/${MESH_GAP_RETRIES})`
  );
  try {
    await client.send(transfer.from_amid, {
      type: "mesh:transfer_chunk_request",
      transfer_id: transfer.transfer_id,
      missing,
      from_agent: process.env.SANDBOX_NAME || "unknown",
      timestamp: new Date().toISOString(),
    });
  } catch (e: unknown) {
    log?.warn(
      `Mesh transfer ${transfer.transfer_id.slice(0, 8)}: chunk_request send failed: ` +
      `${(e as Error)?.message || e}`
    );
  }
  // Re-arm so we try one more time if no progress in another GAP_DETECT_MS window.
  armGapTimer(client, key, log);
}

/**
 * Receiver-side helper: pull any orphan chunks for this transfer (received
 * before the manifest) and insert them into the freshly created pending
 * transfer entry.
 */
async function adoptOrphanChunks(
  transfer: PendingMeshTransfer,
  key: string,
  log?: MeshLogger,
): Promise<void> {
  const orphan = orphanChunks.get(key);
  if (!orphan) return;
  const { createHash } = await import("node:crypto");
  let adopted = 0;
  for (const [idx, { data, hash }] of orphan.chunks) {
    const expected = transfer.chunk_hashes[idx];
    if (expected) {
      const computed = createHash("sha256").update(data).digest("hex");
      if (computed !== expected) continue;
    } else if (hash) {
      const computed = createHash("sha256").update(data).digest("hex");
      if (computed !== hash) continue;
    }
    transfer.chunks.set(idx, data);
    adopted += 1;
  }
  orphanChunks.delete(key);
  if (adopted > 0) {
    log?.info(
      `Mesh transfer ${transfer.transfer_id.slice(0, 8)}: adopted ${adopted} orphan chunk(s) ` +
      `received before manifest`
    );
  }
}

/**
 * Sender-side: handle a receiver's request for missing chunks by resending
 * them from the outbound cache.
 */
async function handleChunkRequest(
  client: MeshSendClient,
  fromAmid: string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  message: any,
  log?: MeshLogger,
): Promise<void> {
  const transferId = message?.transfer_id;
  const missing: unknown = message?.missing;
  if (typeof transferId !== "string" || !Array.isArray(missing)) return;
  const cached = outboundTransfers.get(transferId);
  if (!cached) {
    log?.warn(
      `Mesh chunk_request for unknown/expired transfer ${transferId.slice(0, 8)} ` +
      `from ${fromAmid.slice(0, 12)} — ignoring`
    );
    return;
  }
  if (cached.target_amid !== fromAmid) {
    // Defensive: only honor requests from the original target — prevents
    // a malicious peer from extracting another peer's chunked payload.
    log?.warn(
      `Mesh chunk_request for transfer ${transferId.slice(0, 8)} from non-target ` +
      `${fromAmid.slice(0, 12)} (expected ${cached.target_amid.slice(0, 12)}) — refused`
    );
    return;
  }
  const indices = missing
    .filter((i): i is number => typeof i === "number" && i >= 0 && i < cached.total_chunks)
    .slice(0, cached.total_chunks);
  log?.info(
    `Mesh transfer ${transferId.slice(0, 8)}: resending ${indices.length} chunk(s) ` +
    `at receiver request`
  );
  for (const i of indices) {
    try {
      await client.send(fromAmid, {
        type: "mesh:transfer_chunk",
        transfer_id: transferId,
        chunk_index: i,
        total_chunks: cached.total_chunks,
        data: cached.chunk_data[i],
        hash: cached.chunk_hashes[i],
        from_agent: cached.from_agent,
      });
    } catch (e: unknown) {
      log?.warn(
        `Mesh transfer ${transferId.slice(0, 8)}: resend chunk ${i} failed: ` +
        `${(e as Error)?.message || e}`
      );
    }
  }
}

/**
 * Handle an inbound message that may or may not be part of the chunked
 * transport layer. Returns:
 *   - undefined → not a transport message; app layer should handle.
 *   - null      → absorbed (manifest stored, partial chunk, or reject).
 *   - object    → fully reassembled application-layer message.
 *
 * `client` is the mesh send client used for receiver-initiated retransmit
 * requests and completion acks. May be undefined for unit tests / legacy
 * call sites — recovery features are no-ops in that case.
 */
export async function meshHandleTransportMessage(
  fromAmid: string,
  fromAgent: string,
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  message: any,
  _log?: MeshLogger,
  client?: MeshSendClient,
): Promise<Record<string, unknown> | null | undefined> {
  const msgType = message?.type;
  if (
    msgType !== "mesh:transfer_manifest" &&
    msgType !== "mesh:transfer_chunk" &&
    msgType !== "mesh:transfer_chunk_request" &&
    msgType !== "mesh:transfer_complete"
  ) {
    return undefined;
  }

  const transferId = message.transfer_id;
  if (!transferId) return null;

  // Sender-side: receiver is asking for chunks they didn't get.
  if (msgType === "mesh:transfer_chunk_request") {
    if (client) await handleChunkRequest(client, fromAmid, message, _log);
    return null;
  }

  // Sender-side: receiver completed reassembly — free the outbound cache.
  if (msgType === "mesh:transfer_complete") {
    if (outboundTransfers.delete(transferId)) {
      _log?.info(
        `Mesh transfer ${transferId.slice(0, 8)}: receiver confirmed completion — cache freed`
      );
    }
    return null;
  }

  const key = `${fromAmid}:${transferId}`;

  if (msgType === "mesh:transfer_manifest") {
    // Idempotent: a duplicate manifest (e.g. sender retried at app layer)
    // should not wipe partial chunks already accumulated.
    let transfer = pendingTransfers.get(key);
    if (!transfer) {
      transfer = {
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
        gap_retries: 0,
      };
      pendingTransfers.set(key, transfer);
    } else {
      transfer.received_at = Date.now();
    }
    await adoptOrphanChunks(transfer, key, _log);
    _log?.info(
      `Mesh transfer manifest: ${message.total_chunks} chunks, ` +
      `${(message.total_bytes / 1024).toFixed(0)} KB (transfer ${transferId.slice(0, 8)})`
    );
    if (client && transfer.chunks.size < transfer.total_chunks) armGapTimer(client, key, _log);
    if (transfer.chunks.size >= transfer.total_chunks) {
      // All chunks were already orphaned — reassemble immediately.
      return await finalizeReassembly(transfer, key, fromAmid, _log, client);
    }
    return null;
  }

  // mesh:transfer_chunk
  const transfer = pendingTransfers.get(key);
  if (!transfer) {
    // Orphan: chunk arrived before the manifest. Buffer briefly so a
    // delayed manifest can adopt it. Previously this was silently dropped
    // and was a known cause of failed binary transfers when the SDK
    // delivered chunks out-of-order across a Double Ratchet step.
    let orphan = orphanChunks.get(key);
    if (!orphan) {
      orphan = { chunks: new Map(), first_seen: Date.now() };
      orphanChunks.set(key, orphan);
    }
    orphan.chunks.set(message.chunk_index, {
      data: String(message.data ?? ""),
      hash: typeof message.hash === "string" ? message.hash : undefined,
    });
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
    if (client) armGapTimer(client, key, _log);
    return null;
  }

  return await finalizeReassembly(transfer, key, fromAmid, _log, client);
}

async function finalizeReassembly(
  transfer: PendingMeshTransfer,
  key: string,
  fromAmid: string,
  log?: MeshLogger,
  client?: MeshSendClient,
): Promise<Record<string, unknown> | null> {
  if (transfer.gap_timer) {
    clearTimeout(transfer.gap_timer);
    transfer.gap_timer = undefined;
  }
  const transferId = transfer.transfer_id;
  log?.info(
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
    log?.warn(
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
    log?.warn(`Mesh transfer ${transferId.slice(0, 8)}: reassembled JSON parse failed`);
    return null;
  }

  log?.info(
    `Mesh transfer ${transferId.slice(0, 8)}: reassembled ${(reassembledJson.length / 1024).toFixed(0)} KB ` +
    `(type: ${reassembled.type || transfer.metadata.original_type})`
  );

  if (client && !transfer.ack_sent) {
    transfer.ack_sent = true;
    void sendTransferComplete(client, fromAmid, transferId, transfer.from_agent, log);
  }

  return reassembled;
}
