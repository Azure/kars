// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "yaml";
import { describe, expect, it } from "vitest";

const root = process.env.KARS_TEST_REPO_ROOT ?? fileURLToPath(new URL("../../../", import.meta.url));
const revision = `sha256:${"a".repeat(64)}`;

function fixture() {
  return {
    deployment: {
      apiVersion: "apps/v1", kind: "Deployment",
      metadata: { name: "kars-controller" },
      spec: { template: { spec: { containers: [
        { name: "controller", image: "controller:latest", env: [
          { name: "AZURE_OPENAI_ENDPOINT", value: "https://old.example" },
          { name: "AZURE_OPENAI_API_KEY", valueFrom: { secretKeyRef: { name: "bootstrap", key: "key" } } },
          { name: "INFERENCE_ROUTER_IMAGE", value: "router:latest" },
        ] },
        { name: "other", env: [{ name: "AZURE_OPENAI_ENDPOINT", value: "unrelated" }] },
      ] } } },
    },
    live: {
      metadata: { uid: "controller-uid" },
      spec: { template: { metadata: { annotations: { "kars.azure.com/credential-settings-revision": revision } } } },
    },
    settings: {
      metadata: { uid: "settings-uid", resourceVersion: "123" },
      data: { configuration: Buffer.from(JSON.stringify([
        { name: "AZURE_OPENAI_ENDPOINT", value: "https://selected.example" },
        { name: "AZURE_OPENAI_API_KEY", remove: true },
        { name: "COPILOT_GITHUB_TOKEN", secret: { name: "copilot", uid: "credential-uid", key: "token" } },
      ])).toString("base64") },
    },
    grants: { items: [{
      metadata: { generation: 2 },
      spec: {
        controller: { name: "kars-controller", uid: "controller-uid" },
        integrationStores: [{
          purpose: "controller-settings",
          secret: { name: "kars-credential-controller-settings", uid: "settings-uid" },
        }],
      },
      status: {
        phase: "Ready", observedGeneration: 2,
        integrationRevision: `kars-credential-controller-settings:settings-uid:123,${revision}`,
      },
    }] },
  };
}

function render(input: unknown) {
  const directory = mkdtempSync(join(tmpdir(), "kars-controller-settings-"));
  try {
    mkdirSync(join(directory, "templates"));
    writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: settings-test\nversion: 0.1.0\n");
    writeFileSync(join(directory, "values.yaml"), JSON.stringify({ input }));
    copyFileSync(join(root, "deploy/helm/kars/templates/_controller-operator-settings.tpl"),
      join(directory, "templates/_settings.tpl"));
    writeFileSync(join(directory, "templates/controller.yaml"),
      '{{ include "kars.controller.operatorSettings" .Values.input }}\n');
    return parse(execFileSync("helm", ["template", "settings-test", directory], {
      encoding: "utf8", timeout: 10_000, stdio: ["ignore", "pipe", "pipe"],
    }));
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

describe("operator-owned controller settings during Helm upgrades", () => {
  it("keeps bootstrap settings for a fresh installation", () => {
    const input = fixture();
    expect(render({ ...input, live: {}, settings: {}, grants: {} })).toEqual(input.deployment);
  });

  it("keeps bootstrap settings until the credential authority has applied configuration", () => {
    const input = fixture();
    input.live.spec.template.metadata.annotations["kars.azure.com/credential-settings-revision"] = "";
    expect(render(input)).toEqual(input.deployment);
  });

  it("renders current operator settings, removals, and credential references without changing images", () => {
    const input = fixture();
    const output = render(input);
    expect(output.spec.template.spec.containers[0]).toEqual({
      name: "controller", image: "controller:latest",
      env: [
        { name: "INFERENCE_ROUTER_IMAGE", value: "router:latest" },
        { name: "AZURE_OPENAI_ENDPOINT", value: "https://selected.example" },
        { name: "COPILOT_GITHUB_TOKEN", valueFrom: { secretKeyRef: { name: "copilot", key: "token" } } },
      ],
    });
    expect(output.spec.template.spec.containers[1]).toEqual(input.deployment.spec.template.spec.containers[1]);
    expect(JSON.stringify(output)).not.toContain("credential-uid");
    expect(JSON.stringify(output)).not.toContain("bootstrap");
  });

  it.each([
    ["replaced controller", (input: ReturnType<typeof fixture>) => { input.live.metadata.uid = "replacement"; }],
    ["replaced settings Secret", (input: ReturnType<typeof fixture>) => { input.settings.metadata.uid = "replacement"; }],
    ["unreconciled settings version", (input: ReturnType<typeof fixture>) => { input.settings.metadata.resourceVersion = "124"; }],
    ["unreconciled grant", (input: ReturnType<typeof fixture>) => { input.grants.items[0].metadata.generation = 3; }],
    ["degraded grant", (input: ReturnType<typeof fixture>) => { input.grants.items[0].status.phase = "Degraded"; }],
    ["failed integration", (input: ReturnType<typeof fixture>) => { Object.assign(input.grants.items[0].status, { integrationError: "not applied" }); }],
    ["missing applied evidence", (input: ReturnType<typeof fixture>) => { input.grants.items[0].status.integrationRevision = "kars-credential-controller-settings:settings-uid:123"; }],
  ] as const)("refuses an upgrade with %s rather than resetting operator configuration", (_name, mutate) => {
    const input = fixture();
    mutate(input);
    expect(() => render(input)).toThrow(/credential authority is not reconciled/);
  });

  it.each(["{}", '{"name":', '[{"name":"X","value":""},{"name":"X","value":""}]',
    '[{"name":"COPILOT_GITHUB_TOKEN","value":"must-not-render"}]'])("rejects malformed configuration %s", configuration => {
    const input = fixture();
    input.settings.data.configuration = Buffer.from(configuration).toString("base64");
    expect(() => render(input)).toThrow();
  });
});
