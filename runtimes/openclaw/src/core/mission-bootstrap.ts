// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { acquireMissionWriter, missionIdentity } from "@kars/mesh/dist/mission-identity.js";
import type { MeshIdentity } from "@kars/mesh/dist/identity.js";
import { missionContract, parseMissionContract, type MissionContract, type MissionTarget } from "@kars/mesh/dist/mission-protocol.js";
import { missionTargetFromEnvironment } from "./mission-receiver.js";

const WRITER_KEY = Symbol.for("kars-mission-prekey-writer");

export interface MissionRuntimeBinding {
  readonly identity: MeshIdentity;
  readonly target: Omit<MissionTarget, "runNonce">;
  readonly contract: MissionContract;
}

function installedContract(value: string | undefined): MissionContract {
  if (value === undefined) return missionContract();
  try {
    if (Buffer.byteLength(value, "utf8") > 8192) throw new Error("Contract exceeds configuration bound");
    const contract = parseMissionContract(JSON.parse(value));
    if (contract) return contract;
  } catch { /* Invalid present configuration must never select legacy execution. */ }
  throw new Error("Invalid KARS_MISSION_CONTRACT configuration");
}

/** Called inside the existing AGT init singleton, before identity persistence or SDK construction. */
export async function initializeMissionBinding(environment = process.env): Promise<MissionRuntimeBinding | null> {
  const env = { ...environment };
  if (env.KARS_MISSION_DISPATCH_ENABLED !== "true") return null;
  const owner = process as typeof process & { [WRITER_KEY]?: ReturnType<typeof acquireMissionWriter> };
  if (owner[WRITER_KEY]) throw new Error("Mission identity initialization already attempted; restart the process");

  // Keep custody even if validation or later SDK initialization fails. Only process exit releases it.
  const writer = acquireMissionWriter();
  owner[WRITER_KEY] = writer;
  await writer;
  const identity = missionIdentity(env.KARS_MISSION_IDENTITY_ROOT ?? "", "runtime",
    env.KARS_MISSION_SANDBOX_UID ?? "", env.KARS_MISSION_POD_UID ?? "");
  const target = missionTargetFromEnvironment(identity.did, env);
  if (!target) throw new Error("Mission binding was disabled during initialization");
  return Object.freeze({ identity, target, contract: installedContract(env.KARS_MISSION_CONTRACT) });
}
