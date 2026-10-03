// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { MissionTarget } from "@kars/mesh/dist/mission-protocol.js";

type KnockHandler = (from: string, request: unknown) => Promise<{ accept: boolean }>;

export function missionKnockHandler(target: Omit<MissionTarget, "runNonce"> | null, fallback: KnockHandler): KnockHandler {
  // Session admission is not execution authorization: the receiver still checks encryption,
  // exact target/boot/assignment and task:execute before running a mission.
  return async (from, request) => target?.dispatcherDid === from ? { accept: true } : fallback(from, request);
}
