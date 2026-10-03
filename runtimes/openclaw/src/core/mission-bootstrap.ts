// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { acquireMissionWriter, missionIdentity } from "@kars/mesh/dist/mission-identity.js";
import type { MeshIdentity } from "@kars/mesh/dist/identity.js";
import { missionTargetFromEnvironment } from "./mission-receiver.js";

const WRITER_KEY = Symbol.for("kars-mission-prekey-writer");

/** Called inside the existing AGT init singleton, before identity persistence or SDK construction. */
export async function initializeMissionIdentity(environment = process.env): Promise<MeshIdentity | null> {
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
  missionTargetFromEnvironment(identity.did, env);
  return identity;
}
