// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Coherent timer ladder for all mesh operations.
 *
 * Every timeout is expressed as a tier, not a magic number, so that:
 *   1. Related operations scale together (e.g. if the relay is slow, all
 *      roundtrip-class timers stretch, not just one).
 *   2. Ops can scale the whole ladder uniformly via `MESH_TIMEOUT_SCALE`
 *      (e.g. `MESH_TIMEOUT_SCALE=2` for slow networks or Kata/confidential
 *      sandboxes where cold start can reach 90–120 s).
 *   3. Invariants between timers are preserved.  The offload code, for
 *      example, must wait longer for the peer's HELLO than the peer waits
 *      to establish its mesh connection.
 *
 * Tier ladder (strictly increasing; each tier covers the previous tier's
 * worst case plus its own work):
 *
 *   T1 PING         — single RPC on an established mesh connection
 *   T1 ACK          — single result-delivery ack
 *   T2 RELAY        — WebSocket handshake to relay (fresh connection)
 *   T2 PAIR         — controller pair-ack (one RPC over fresh connection)
 *   T3 PROGRESS     — remote peer performing a unit of work
 *   T4 COLD_START   — remote sandbox cold-booting from zero
 *                     (covers pod schedule + image pull + entrypoint +
 *                      relay dial + first HELLO)
 */

const RAW_SCALE = Number(process.env.MESH_TIMEOUT_SCALE ?? 1);
const SCALE = Number.isFinite(RAW_SCALE) && RAW_SCALE > 0 ? RAW_SCALE : 1;

const ms = (n: number) => Math.max(1_000, Math.round(n * SCALE));

export const TIMEOUTS = {
  // ── T1: single RPC on established mesh connection ──────────────
  PING: ms(5_000),
  ACK: ms(8_000),

  // ── T2: one roundtrip, fresh mesh connection ───────────────────
  RELAY_CONNECT: ms(10_000),
  PAIR_HANDSHAKE: ms(10_000),

  // ── T3: a unit of work on the peer ─────────────────────────────
  PROGRESS: ms(30_000),

  // ── T4: cold-start a remote sandbox ────────────────────────────
  // Covers pod schedule + image pull + entrypoint (incl. token
  // retry budget of 120s) + relay dial + first HELLO.  The 180s
  // default gives comfortable headroom over the 120s token retry
  // alone; Kata/confidential sandboxes can reach this under load.
  COLD_START: ms(180_000),
} as const;

export const RETRIES = {
  // Total budget ≈ (timeout + delay) × count ≈ 3 × (5+2) = 21s → spans T2
  PING: { count: 3, delayMs: ms(2_000) },
  // Total budget ≈ 2 × (8+2) = 20s
  ACK: { count: 2, delayMs: ms(2_000) },
  // Used when we want to survive a short progress gap: 3 × (30+2) = 96s
  PROGRESS: { count: 3, delayMs: ms(2_000) },
} as const;

/** For log/diagnostic output only. */
export function describeTimers(): Record<string, string> {
  return {
    scale: String(SCALE),
    ping: `${TIMEOUTS.PING}ms × ${RETRIES.PING.count}`,
    ack: `${TIMEOUTS.ACK}ms × ${RETRIES.ACK.count}`,
    relay: `${TIMEOUTS.RELAY_CONNECT}ms`,
    pair: `${TIMEOUTS.PAIR_HANDSHAKE}ms`,
    progress: `${TIMEOUTS.PROGRESS}ms × ${RETRIES.PROGRESS.count}`,
    cold_start: `${TIMEOUTS.COLD_START}ms`,
  };
}
