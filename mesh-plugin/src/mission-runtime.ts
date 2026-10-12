// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

export type MissionRuntime = "OpenClaw" | "Hermes";

export function missionRuntime(spec: {
  blueprint?: { runtime?: string }; execution?: { runtime?: string };
}): MissionRuntime | null {
  const runtime = spec.blueprint?.runtime ?? spec.execution?.runtime ?? "OpenClaw";
  return runtime === "OpenClaw" || runtime === "Hermes" ? runtime : null;
}
