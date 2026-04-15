/**
 * Pairing ceremony — decode token, connect to mesh, pair with controller.
 */

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

const PAIRINGS_DIR = path.join(os.homedir(), ".azureclaw");
const PAIRINGS_FILE = path.join(PAIRINGS_DIR, "pairings.json");

const TOKEN_PREFIX = "azcp_1_";

export interface PairingTokenPayload {
  controller_amid: string;
  relay_url: string;
  registry_url: string;
  secret: string;
}

export interface StoredPairing {
  controllerAmid: string;
  relayUrl: string;
  registryUrl: string;
  clusterName: string;
  capabilities: string[];
  slots: number;
  tokenBudget: number;
  expiresAt: string;
  pairedAt: string;
}

/** Decode an azcp_1_ pairing token. Returns null if invalid. */
export function decodeToken(token: string): PairingTokenPayload | null {
  // Strip any whitespace/newlines that chat UIs may inject
  const cleaned = token.replace(/\s+/g, "").trim();
  if (!cleaned.startsWith(TOKEN_PREFIX)) return null;
  try {
    const b64 = cleaned.slice(TOKEN_PREFIX.length);
    const json = Buffer.from(b64, "base64url").toString("utf-8");
    const payload = JSON.parse(json);
    if (!payload.controller_amid || !payload.secret) return null;
    return payload as PairingTokenPayload;
  } catch {
    return null;
  }
}

/** Load stored pairings from disk. */
export function loadPairings(): Record<string, StoredPairing> {
  if (!fs.existsSync(PAIRINGS_FILE)) return {};
  try {
    return JSON.parse(fs.readFileSync(PAIRINGS_FILE, "utf-8"));
  } catch {
    return {};
  }
}

/** Save a new pairing. */
export function savePairing(clusterName: string, pairing: StoredPairing): void {
  fs.mkdirSync(PAIRINGS_DIR, { recursive: true });
  const pairings = loadPairings();
  pairings[clusterName] = pairing;
  fs.writeFileSync(PAIRINGS_FILE, JSON.stringify(pairings, null, 2), { mode: 0o600 });
}

/** Get the default (most recent) pairing. */
export function getDefaultPairing(): StoredPairing | null {
  const pairings = loadPairings();
  const entries = Object.values(pairings);
  if (entries.length === 0) return null;
  // Return most recently paired
  return entries.sort((a, b) =>
    new Date(b.pairedAt).getTime() - new Date(a.pairedAt).getTime()
  )[0];
}
