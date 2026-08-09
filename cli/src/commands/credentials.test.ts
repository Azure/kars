// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import {
  CREDENTIAL_FLAG_TO_ENV,
  buildFeishuChannelSecretPatch,
  buildFeishuRotationSecretName,
  planCredentialSecretUpdates,
  selectSandboxForCredentialUpdate,
  validateCredentialUpdates,
} from "./credentials.js";
import {
  classifyFeishuSecretReference,
  shouldCleanupStagedFeishuSecret,
} from "../lib/feishu-secret-reference.js";

describe("credentials update channel mappings", () => {
  it("maps Feishu credential flags to runtime environment variables", () => {
    expect(CREDENTIAL_FLAG_TO_ENV.feishuAppId).toBe("FEISHU_APP_ID");
    expect(CREDENTIAL_FLAG_TO_ENV.feishuAppSecret).toBe("FEISHU_APP_SECRET");
  });

  it("requires Feishu App ID and App Secret to rotate together", () => {
    expect(() => validateCredentialUpdates({ FEISHU_APP_ID: "cli_new" })).toThrow(
      "must be updated together",
    );
    expect(() =>
      validateCredentialUpdates({
        FEISHU_APP_ID: "cli_new",
        FEISHU_APP_SECRET: "secret",
      }),
    ).not.toThrow();
    expect(() =>
      validateCredentialUpdates({
        FEISHU_APP_ID: "cli_new",
        FEISHU_APP_SECRET: "secret",
        TELEGRAM_BOT_TOKEN: "telegram",
      }),
    ).toThrow("separately");
  });

  it("routes Feishu and ordinary credentials to separate declared targets", () => {
    const sandbox = {
      spec: {
        channels: [{ type: "Feishu", credentialSecretRef: { name: "custom-feishu" } }],
      },
    };
    expect(planCredentialSecretUpdates(
      "agent",
      sandbox,
      { TELEGRAM_BOT_TOKEN: "telegram" },
    )).toEqual([
      { kind: "conventional", secretName: "agent-credentials", updates: { TELEGRAM_BOT_TOKEN: "telegram" } },
    ]);
    expect(planCredentialSecretUpdates(
      "agent",
      sandbox,
      {
        FEISHU_APP_ID: "cli_new",
        FEISHU_APP_SECRET: "secret",
      },
    )).toEqual([
      {
        kind: "feishu",
        secretName: "custom-feishu",
        updates: { FEISHU_APP_ID: "cli_new", FEISHU_APP_SECRET: "secret" },
      },
    ]);
    expect(() => planCredentialSecretUpdates(
      "agent",
      { spec: { channels: [] } },
      { FEISHU_APP_ID: "cli_new", FEISHU_APP_SECRET: "secret" },
    )).toThrow("does not declare a Feishu channel");
  });

  it("builds an immutable Secret name and non-sensitive channel-ref patch", () => {
    expect(buildFeishuRotationSecretName("custom-feishu", "a1b2c3")).toBe(
      "custom-feishu-rotation-a1b2c3",
    );
    const patch = buildFeishuChannelSecretPatch(
      "12345",
      [{ type: "Feishu", feishu: { domain: "Feishu" } }],
      "custom-feishu-rotation-a1b2c3",
    );
    expect(patch).toEqual([
      { op: "test", path: "/metadata/resourceVersion", value: "12345" },
      { op: "test", path: "/spec/channels/0/type", value: "Feishu" },
      {
        op: "add",
        path: "/spec/channels/0/credentialSecretRef",
        value: { name: "custom-feishu-rotation-a1b2c3" },
      },
    ]);
    expect(JSON.stringify(patch)).not.toContain("FEISHU_APP");

    expect(buildFeishuChannelSecretPatch(
      "12346",
      [{ type: "Feishu", credentialSecretRef: { name: "old" } }],
      "new",
    )).toEqual([
      { op: "test", path: "/metadata/resourceVersion", value: "12346" },
      { op: "test", path: "/spec/channels/0/type", value: "Feishu" },
      {
        op: "replace",
        path: "/spec/channels/0/credentialSecretRef/name",
        value: "new",
      },
    ]);
  });

  it("selects one namespaced sandbox and rejects ambiguous names", () => {
    const selected = selectSandboxForCredentialUpdate("agent", {
      items: [{ metadata: { name: "agent", namespace: "team-a" } }],
    });
    expect(selected.metadata?.namespace).toBe("team-a");
    expect(() =>
      selectSandboxForCredentialUpdate("agent", {
        items: [
          { metadata: { name: "agent", namespace: "team-a" } },
          { metadata: { name: "agent", namespace: "team-b" } },
        ],
      }),
    ).toThrow("multiple KarsSandboxes");
  });

  it("preserves a staged Secret when an ambiguous patch committed or cannot be checked", () => {
    const committed = JSON.stringify({
      spec: { channels: [{ type: "Feishu", credentialSecretRef: { name: "new-revision" } }] },
    });
    const conflict = JSON.stringify({
      spec: { channels: [{ type: "Feishu", credentialSecretRef: { name: "old-revision" } }] },
    });
    expect(classifyFeishuSecretReference(committed, "new-revision")).toBe("referenced");
    expect(classifyFeishuSecretReference(conflict, "new-revision")).toBe("unreferenced");
    expect(classifyFeishuSecretReference("", "new-revision")).toBe("unreferenced");
    expect(classifyFeishuSecretReference("not-json", "new-revision")).toBe("unknown");
    expect(shouldCleanupStagedFeishuSecret("new-revision", "referenced")).toBe(false);
    expect(shouldCleanupStagedFeishuSecret("new-revision", "unreferenced")).toBe(true);
    expect(shouldCleanupStagedFeishuSecret("new-revision", "unknown")).toBe(false);
    expect(shouldCleanupStagedFeishuSecret(undefined, "unreferenced")).toBe(false);
  });
});
