// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { missionPhaseDigest } from "./mission-phase.js";
import { parseMissionMessage, type MissionReply } from "./mission-protocol.js";

const fixtures: { name: string; reply: MissionReply }[] = JSON.parse(readFileSync(new URL(
  "../../bridge/bff/src/routes/tasks/run_evidence/protocol-fixtures.json", import.meta.url,
), "utf8"));

describe("Bridge terminal evidence contract fixtures", () => {
  it.each(fixtures)("keeps $name compatible with the actual wire parser", ({ reply }) => {
    const parsed = parseMissionMessage(reply);
    expect(parsed?.type).toBe("mission:reply");
    expect(parsed).not.toHaveProperty("inputArtifacts");
    if (reply.version !== 1) {
      expect(reply.phaseDigest).toBe(missionPhaseDigest(reply.reviewedPhase));
    }
  });
});
