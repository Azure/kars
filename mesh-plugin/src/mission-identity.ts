// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHmac } from "node:crypto";
import { createServer } from "node:net";
import { identityFromSigningSeed, type MeshIdentity } from "./identity.js";

export type MissionIdentityRole = "runtime" | "dispatcher";
const component = (value: string): boolean => typeof value === "string" && /^[a-zA-Z0-9][a-zA-Z0-9._-]{0,252}$/.test(value);

/** The root must come from a controller/installer-owned Secret, never a public Entra app ID. */
export function missionIdentitySeed(root: string, role: MissionIdentityRole, ownerUid: string, podUid: string): Buffer {
  if (typeof root !== "string" || !/^[a-fA-F0-9]{64}$/.test(root) || !["runtime", "dispatcher"].includes(role) || !component(ownerUid) || !component(podUid)) {
    throw new Error("Mission identity requires a 32-byte Secret root, role, owner UID and Pod UID");
  }
  return createHmac("sha256", Buffer.from(root, "hex"))
    .update(JSON.stringify(["kars-mission-identity-v1", role, ownerUid, podUid])).digest();
}

export function missionIdentity(root: string, role: MissionIdentityRole, ownerUid: string, podUid: string): MeshIdentity {
  return identityFromSigningSeed(missionIdentitySeed(root, role, ownerUid, podUid));
}

/** A Pod shares its network namespace across processes/containers; no filesystem lock can be unlinked. */
export async function acquireMissionWriter(port = 19791): Promise<{ close(): Promise<void> }> {
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid mission writer lock port");
  const server = createServer(socket => socket.destroy());
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen({ host: "127.0.0.1", port, exclusive: true }, () => {
      server.removeListener("error", reject);
      resolve();
    });
  });
  // Retained until the owner exits (or explicitly closes after disconnect); clients cannot release it.
  server.unref();
  let closing: Promise<void> | undefined;
  return {
    close: () => closing ??= new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  };
}
