// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { missionProcessConfig } from "./mission-config.js";
const env = {
  KARS_MISSION_AUTH_MODE: "development", POD_NAMESPACE: "kars-system", POD_NAME: "dispatcher-abc", POD_UID: "pod-uid",
  KARS_MISSION_DISPATCHER_DEPLOYMENT: "kars-mission-dispatcher", KARS_MISSION_DISPATCHER_RELEASE: "kars",
  KARS_MISSION_IDENTITY_ROOT: "a".repeat(64), AGENTMESH_RELAY_URL: "ws://relay:8765", AGENTMESH_REGISTRY_URL: "http://registry:8080",
};
describe("mission process configuration", () => {
  it("uses finite timing defaults and a required mounted root", () => {
    const result = missionProcessConfig(env);
    expect(result.service.root).toBe(env.KARS_MISSION_IDENTITY_ROOT);
    expect(result.service.timeoutMs).toBe(300_000); expect(result.service.pollMs).toBe(2_000);
    expect(result.port).toBe(8080); expect(result.shutdownMs).toBe(30_000);
  });
  it.each([
    ["ws://relay:8765", "ws://relay:8765"],
    ["ws://relay:8765/", "ws://relay:8765"],
    ["wss://relay.example/", "wss://relay.example"],
    ["ws://relay:8765/agt/relay/", "ws://relay:8765/agt/relay"],
    ["ws://relay:8765/ws", "ws://relay:8765/ws"],
  ])("keeps %s compatible with the SDK relay path", (input, expected) => {
    const config = missionProcessConfig({ ...env, AGENTMESH_RELAY_URL: input });
    expect(config.service.relayUrl).toBe(expected);
    expect(config.service.registryUrl).toBe("http://registry:8080");
  });
  it.each(Object.keys(env))("rejects missing %s", key => {
    const input: NodeJS.ProcessEnv = { ...env }; delete input[key];
    expect(() => missionProcessConfig(input)).toThrow();
  });
  it.each(["entra", "anonymous", "", "false"])("never downgrades unsupported auth mode %s", mode => {
    expect(() => missionProcessConfig({ ...env, KARS_MISSION_AUTH_MODE: mode })).toThrow("explicit development");
  });
  it.each([
    ["POD_NAMESPACE", "wrong/namespace"], ["POD_UID", "../../wrong"], ["KARS_MISSION_IDENTITY_ROOT", "not a root"],
    ["AGENTMESH_RELAY_URL", "http://relay"], ["AGENTMESH_RELAY_URL", "ws://user:private@relay"],
    ["AGENTMESH_REGISTRY_URL", "http://registry/?token=private"], ["AGENTMESH_REGISTRY_URL", "https://registry/#private"],
    ["PORT", "0"], ["PORT", "65536"], ["PORT", "1e3"], ["KARS_MISSION_POLL_MS", "99"],
    ["KARS_MISSION_TIMEOUT_MS", "300001"], ["KARS_MISSION_SHUTDOWN_MS", "30001"],
  ])("rejects invalid %s without reflecting its value", (key, value) => {
    expect(() => missionProcessConfig({ ...env, [key]: value })).toThrow("Invalid mission dispatcher configuration");
  });
});
