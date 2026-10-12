// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { missionProcessConfig } from "./mission-config.js";
const env = {
  KARS_MISSION_AUTH_MODE: "development", POD_NAMESPACE: "kars-system", POD_NAME: "dispatcher-abc", POD_UID: "pod-uid",
  KARS_MISSION_DISPATCHER_DEPLOYMENT: "kars-mission-dispatcher", KARS_MISSION_DISPATCHER_RELEASE: "kars",
  KARS_MISSION_IDENTITY_ROOT: "a".repeat(64), AGENTMESH_RELAY_URL: "ws://relay:8765", AGENTMESH_REGISTRY_URL: "http://registry:8080",
};
const entraEnv = {
  ...env, KARS_MISSION_AUTH_MODE: "entra",
  KARS_MISSION_TENANT_ID: "a1111111-1111-4111-8111-111111111111",
  KARS_MISSION_BLUEPRINT_ID: "b2222222-2222-4222-8222-222222222222",
};

describe("mission process Entra configuration", () => {
  it("selects required Entra explicitly with pinned tenant and blueprint", () => {
    const result = missionProcessConfig(entraEnv);
    expect(result.service).toEqual({
      ...missionProcessConfig(env).service,
      entra: { tenantId: entraEnv.KARS_MISSION_TENANT_ID, blueprintId: entraEnv.KARS_MISSION_BLUEPRINT_ID },
    });
  });
  it.each(["KARS_MISSION_TENANT_ID", "KARS_MISSION_BLUEPRINT_ID"])("requires a lowercase GUID for %s", key => {
    for (const value of [undefined, "", "not-a-guid", "a1111111111141118111111111111111", "A1111111-1111-4111-8111-111111111111", " a1111111-1111-4111-8111-111111111111"]) {
      expect(() => missionProcessConfig({ ...entraEnv, [key]: value })).toThrow(/^Invalid mission dispatcher configuration$/);
    }
  });
  it.each(["AGT_SKIP_INIT", "AGT_SKIP_ENTRA", "AGT_SKIP_VERIFY", "AGT_OAUTH_TOKEN"])("rejects any presence of %s in Entra mode", key => {
    for (const value of ["", "0", "false", "1", "private-credential"]) {
      expect(() => missionProcessConfig({ ...entraEnv, [key]: value })).toThrow(/^Invalid mission dispatcher configuration$/);
    }
    expect(() => missionProcessConfig({ ...entraEnv, [key]: undefined })).not.toThrow();
  });
  it("keeps explicit development independent of Entra configuration", () => {
    expect(missionProcessConfig({ ...env, KARS_MISSION_TENANT_ID: "invalid", AGT_SKIP_ENTRA: "1", AGT_OAUTH_TOKEN: "legacy" }))
      .toEqual(missionProcessConfig(env));
  });
});

describe("mission process configuration", () => {
  it("uses finite timing defaults and a required mounted root", () => {
    const result = missionProcessConfig(env);
    expect(result.service.root).toBe(env.KARS_MISSION_IDENTITY_ROOT);
    expect(result.service.timeoutMs).toBe(300_000); expect(result.service.pollMs).toBe(2_000);
    expect(result.port).toBe(8080); expect(result.shutdownMs).toBe(30_000);
    expect(result.service).not.toHaveProperty("entra");
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
  it.each(["anonymous", "", "false", "Entra", "DEVELOPMENT"])("never downgrades unsupported auth mode %s", mode => {
    expect(() => missionProcessConfig({ ...env, KARS_MISSION_AUTH_MODE: mode })).toThrow("explicit development or entra");
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
