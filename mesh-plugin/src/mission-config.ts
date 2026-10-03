// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import type { MissionServiceConfig } from "./mission-service.js";

export interface MissionProcessConfig {
  service: MissionServiceConfig;
  port: number;
  shutdownMs: number;
}

export function missionProcessConfig(env: NodeJS.ProcessEnv): MissionProcessConfig {
  const invalid = () => new Error("Invalid mission dispatcher configuration");
  const required = (key: string, pattern: RegExp): string => {
    const value = env[key];
    if (!value || !pattern.test(value)) throw invalid();
    return value;
  };
  const number = (key: string, fallback: number, min: number, max: number): number => {
    const raw = env[key];
    if (raw !== undefined && !/^[1-9][0-9]*$/.test(raw)) throw invalid();
    const value = raw === undefined ? fallback : Number(raw);
    if (!Number.isSafeInteger(value) || value < min || value > max) throw invalid();
    return value;
  };
  const url = (key: string, protocols: string[]): string => {
    let parsed: URL;
    try { parsed = new URL(env[key] ?? ""); } catch { throw invalid(); }
    if (!protocols.includes(parsed.protocol) || !parsed.hostname || parsed.username || parsed.password || parsed.search || parsed.hash) throw invalid();
    // The official SDK appends /ws to relay base URLs; URL adds a root slash.
    return parsed.toString().replace(/\/+$/, "");
  };
  // Development transport is explicit. Entra cannot silently fall back to anonymous registration.
  if (env.KARS_MISSION_AUTH_MODE !== "development") throw new Error("Mission dispatcher requires explicit development authentication; Entra is not supported yet");
  const name = /^[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?$/;
  const namespace = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;
  return {
    service: {
      location: {
        namespace: required("POD_NAMESPACE", namespace),
        deployment: required("KARS_MISSION_DISPATCHER_DEPLOYMENT", name),
        release: required("KARS_MISSION_DISPATCHER_RELEASE", /^[a-z0-9](?:[a-z0-9-]{0,51}[a-z0-9])?$/),
        podName: required("POD_NAME", name),
        podUid: required("POD_UID", /^[A-Za-z0-9][A-Za-z0-9._-]{0,252}$/),
      },
      root: required("KARS_MISSION_IDENTITY_ROOT", /^[a-fA-F0-9]{64}$/),
      relayUrl: url("AGENTMESH_RELAY_URL", ["ws:", "wss:"]),
      registryUrl: url("AGENTMESH_REGISTRY_URL", ["http:", "https:"]),
      pollMs: number("KARS_MISSION_POLL_MS", 2_000, 100, 30_000),
      timeoutMs: number("KARS_MISSION_TIMEOUT_MS", 300_000, 1_000, 300_000),
    },
    port: number("PORT", 8080, 1, 65535),
    shutdownMs: number("KARS_MISSION_SHUTDOWN_MS", 30_000, 1_000, 30_000),
  };
}
