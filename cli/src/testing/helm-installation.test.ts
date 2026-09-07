// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { parse, parseAllDocuments, stringify } from "yaml";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const chart = join(root, "deploy/helm/kars");
const temporaryDirectories: string[] = [];

interface Container {
  name?: string;
  image?: string;
  env?: Array<{ name: string; value?: string }>;
  command?: string[];
}

interface Manifest {
  kind: string;
  metadata?: { name?: string; labels?: Record<string, string> };
  spec?: {
    replicas?: number;
    strategy?: { type?: string };
    selector?: Record<string, unknown>;
    template?: {
      metadata?: { labels?: Record<string, string> };
      spec?: { containers?: Container[]; initContainers?: Container[] };
    };
  };
}

function documents(text: string): Manifest[] {
  return parseAllDocuments(text).map((document) => {
    if (document.errors.length) throw document.errors[0];
    return document.toJSON();
  }).filter(Boolean);
}

function render(path = chart, args: string[] = []): Manifest[] {
  return documents(execFileSync(
    "helm", ["template", "kars", path, "--namespace", "kars-system", ...args],
    { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 30_000 },
  ));
}

function resource(resources: Manifest[], kind: string, name: string): Manifest {
  const found = resources.find((item) => item.kind === kind && item.metadata?.name === name);
  if (!found) throw new Error(`Missing ${kind}/${name}`);
  return found;
}

function reusedValuesChart(enableMesh = false): string {
  const directory = mkdtempSync(join(tmpdir(), "kars-reused-values-"));
  temporaryDirectories.push(directory);
  const copied = join(directory, "chart");
  cpSync(chart, copied, { recursive: true });
  const savedValues = parse(readFileSync(join(chart, "values.yaml"), "utf8"));
  // --reuse-values replaces the new chart defaults with the saved old map.
  // These two fields did not exist before the generic-installation feature.
  delete savedValues.agentMesh;
  delete savedValues.sandbox.nodeSelector;
  savedValues.controller.replicas = 3;
  savedValues.azure.workloadIdentity.clientId = "existing-customer-identity";
  savedValues.sandbox.image.repository = "registry.customer.example/existing-agent";
  if (enableMesh) savedValues.agentMesh = { enabled: true };
  writeFileSync(join(copied, "values.yaml"), stringify(savedValues));
  return copied;
}

afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

describe("existing Helm installation compatibility", () => {
  it("preserves saved customer values and the legacy selector when new maps are absent", () => {
    const manifests = render(reusedValuesChart());
    expect(manifests.some((item) => item.metadata?.name === "agentmesh-registry")).toBe(false);
    const controller = resource(manifests, "Deployment", "kars-controller");
    expect(controller.spec?.replicas).toBe(3);
    const env = controller.spec?.template?.spec?.containers?.[0]?.env;
    expect(env).toContainEqual({ name: "KARS_SANDBOX_NODE_SELECTOR_JSON", value: "{}" });
    expect(env).toContainEqual({ name: "AZURE_WI_CLIENT_ID", value: "existing-customer-identity" });
    expect(env).toContainEqual({
      name: "SANDBOX_IMAGE", value: "registry.customer.example/existing-agent:latest",
    });
  });

  it("can enable the new mesh using reused values without missing nested defaults", () => {
    const manifests = render(reusedValuesChart(true));
    for (const component of ["registry", "relay"]) {
      const deployment = resource(manifests, "Deployment", component);
      expect(deployment.spec?.replicas).toBe(1);
      expect(deployment.spec?.template?.spec?.containers?.[0]?.image)
        .toBe(`ghcr.io/azure/kars-agentmesh-${component}:latest`);
    }
  });

  it("treats removed/null new configuration as the compatible disabled/default state", () => {
    const manifests = render(chart, ["--set", "agentMesh=null,sandbox.nodeSelector=null"]);
    expect(manifests.some((item) => item.metadata?.name === "agentmesh-registry")).toBe(false);
    expect(resource(manifests, "Deployment", "kars-controller").spec?.template?.spec?.containers?.[0]?.env)
      .toContainEqual({ name: "KARS_SANDBOX_NODE_SELECTOR_JSON", value: "{}" });
  });

  it("uses deployment and service selectors compatible with the standalone AGT manifest", () => {
    const legacy = documents(readFileSync(join(root, "deploy/agentmesh-agt.yaml"), "utf8"));
    const manifests = render(chart, ["--set", "agentMesh.enabled=true"]);
    for (const component of ["registry", "relay"]) {
      const deployment = resource(manifests, "Deployment", component);
      expect(deployment.spec?.selector).toEqual(resource(legacy, "Deployment", component).spec?.selector);
      expect(deployment.spec?.template?.metadata?.labels?.app).toBe(`agentmesh-${component}`);
      expect(deployment.spec?.template?.metadata?.labels?.["app.kubernetes.io/name"])
        .toBe(`agentmesh-${component}`);
      expect(deployment.spec?.strategy?.type).toBe("Recreate");
      const service = `agentmesh-${component}`;
      expect(resource(manifests, "Service", service).spec?.selector)
        .toEqual(resource(legacy, "Service", service).spec?.selector);
    }
  });

  it("allows explicit maintenance scale-down without replacing zero with the default", () => {
    const manifests = render(chart, [
      "--set", "agentMesh.enabled=true,agentMesh.registry.replicas=0,agentMesh.relay.replicas=0",
    ]);
    expect(resource(manifests, "Deployment", "registry").spec?.replicas).toBe(0);
    expect(resource(manifests, "Deployment", "relay").spec?.replicas).toBe(0);
  });

  it.each([
    ["agentMesh.namespace=mesh-custom", /agentMesh.namespace must be agentmesh/],
    ["agentMesh.registry.replicas=2", /replica counts must be 0 or 1/],
    ["agentMesh.relay.replicas=2", /replica counts must be 0 or 1/],
    ["agentMesh.registry.replicas=-1", /replica counts must be 0 or 1/],
  ])("rejects unsupported mesh configuration: %s", (setting, message) => {
    expect(() => render(chart, ["--set", `agentMesh.enabled=true,${setting}`])).toThrow(message);
  });

  it.each(["values-generic.yaml", "values-existing-aks.yaml"])(
    "%s keeps the profile needed by existing/default enhanced sandbox CRs",
    (profile) => {
      const manifests = render(chart, ["--values", join(chart, profile)]);
      const installer = resource(manifests, "DaemonSet", "kars-seccomp-installer");
      const command = installer.spec?.template?.spec?.initContainers?.[0]?.command?.join("\n");
      expect(command).toContain("/host/seccomp/profiles/kars-strict.json");
      expect(command).not.toContain("/host/seccomp/profiles/RuntimeDefault.json");
    },
  );
});
