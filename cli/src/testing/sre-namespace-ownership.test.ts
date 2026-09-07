// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer } from "node:http";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";

const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
interface Resource {
  kind: string;
  metadata: {
    name: string; namespace?: string;
    labels?: Record<string, string>; annotations?: Record<string, string>;
  };
  automountServiceAccountToken?: boolean;
}
function documents(output: string): Resource[] {
  return parseAllDocuments(output).map(document => {
    if (document.errors.length) throw document.errors[0];
    return document.toJSON();
  }).filter(Boolean);
}
function legacy(kind: string, name: string, release = "kars"): Resource {
  return {
    kind,
    metadata: {
      name, ...(kind === "ServiceAccount" ? { namespace: "kars-sre" } : {}),
      labels: { "customer.example/label": "preserve", "app.kubernetes.io/managed-by": "Helm" },
      annotations: {
        "meta.helm.sh/release-name": release,
        "meta.helm.sh/release-namespace": "kars-system",
        "customer.example/annotation": "preserve",
        "kars.azure.com/sandbox-uid": "existing-sandbox-uid",
      },
    },
    ...(kind === "ServiceAccount" ? { automountServiceAccountToken: false } : {}),
  };
}

async function upgrade(
  namespace: Resource | undefined, writer: Resource | undefined, enabled = true, forbidden = false,
): Promise<Resource[]> {
  const server = createServer((request, response) => {
    const path = new URL(request.url!, "http://localhost").pathname;
    response.setHeader("content-type", "application/json");
    if (request.method !== "GET") {
      response.writeHead(405);
      response.end(JSON.stringify({ kind: "Status", reason: "MethodNotAllowed", code: 405 }));
      return;
    }
    let result: unknown;
    if (path === "/version") result = { major: "1", minor: "32", gitVersion: "v1.32.0" };
    if (path === "/api") result = { kind: "APIVersions", apiVersion: "v1", versions: ["v1"] };
    const groups = [
      { name: "kars.azure.com", version: "v1alpha1" },
      { name: "rbac.authorization.k8s.io", version: "v1" },
    ].map(({ name, version }) => ({
      name, versions: [{ groupVersion: `${name}/${version}`, version }],
      preferredVersion: { groupVersion: `${name}/${version}`, version },
    }));
    if (path === "/apis") result = { kind: "APIGroupList", apiVersion: "v1", groups };
    if (path === "/api/v1") result = {
      kind: "APIResourceList", apiVersion: "v1", groupVersion: "v1",
      resources: [
        { name: "namespaces", kind: "Namespace", namespaced: false, verbs: ["get", "list"] },
        { name: "serviceaccounts", kind: "ServiceAccount", namespaced: true, verbs: ["get", "list"] },
      ],
    };
    if (path === "/api/v1/namespaces/kars-sre") result = namespace && { apiVersion: "v1", ...namespace };
    if (path === "/apis/kars.azure.com/v1alpha1") result = {
      kind: "APIResourceList", apiVersion: "v1", groupVersion: "kars.azure.com/v1alpha1",
      resources: [
        { name: "karssandboxes", kind: "KarsSandbox", namespaced: true, verbs: ["get", "list"] },
        { name: "inferencepolicies", kind: "InferencePolicy", namespaced: true, verbs: ["get", "list"] },
        { name: "toolpolicies", kind: "ToolPolicy", namespaced: true, verbs: ["get", "list"] },
      ],
    };
    if (path === "/apis/rbac.authorization.k8s.io/v1") result = {
      kind: "APIResourceList", apiVersion: "v1", groupVersion: "rbac.authorization.k8s.io/v1",
      resources: [
        { name: "clusterroles", kind: "ClusterRole", namespaced: false, verbs: ["get", "list"] },
        { name: "clusterrolebindings", kind: "ClusterRoleBinding", namespaced: false, verbs: ["get", "list"] },
      ],
    };
    if (path === "/api/v1/namespaces/kars-sre/serviceaccounts/sre-writer") {
      result = writer && { apiVersion: "v1", ...writer };
    }
    if (forbidden && path === "/api/v1/namespaces/kars-sre") {
      response.writeHead(403);
      response.end(JSON.stringify({
        kind: "Status", apiVersion: "v1", reason: "Forbidden", code: 403,
        message: "Forbidden: namespace lookup denied",
      }));
      return;
    }
    if (!result) {
      response.writeHead(404);
      result = { kind: "Status", apiVersion: "v1", reason: "NotFound", code: 404 };
    }
    response.end(JSON.stringify(result));
  });
  const directory = mkdtempSync(join(tmpdir(), "kars-sre-lookup-"));
  try {
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Expected TCP test API address");
    const kubeconfig = join(directory, "config");
    const fixtureChart = join(directory, "chart");
    mkdirSync(join(fixtureChart, "templates"), { recursive: true });
    for (const file of ["Chart.yaml", "values.yaml", "templates/sre.yaml"]) {
      copyFileSync(join(chart, file), join(fixtureChart, file));
    }
    writeFileSync(kubeconfig, JSON.stringify({
      apiVersion: "v1", kind: "Config",
      clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${address.port}` } }],
      users: [{ name: "fixture", user: {} }],
      contexts: [{ name: "fixture", context: { cluster: "fixture", user: "fixture" } }],
      "current-context": "fixture",
    }), { mode: 0o600 });
    const { stdout } = await execa("helm", [
      "template", "kars", fixtureChart, "--namespace", "kars-system",
      "--kubeconfig", kubeconfig, "--dry-run=server", "--is-upgrade",
      // The fixture serves discovery and live lookup, not an OpenAPI schema.
      "--disable-openapi-validation",
      "--set", `sre.enabled=${enabled}`, "--show-only", "templates/sre.yaml",
    ], { timeout: 20_000 });
    return documents(stdout);
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    rmSync(directory, { recursive: true, force: true });
  }
}

describe("SRE namespace ownership (actual Helm lookup against an isolated test API)", () => {
  it("leaves fresh runtime namespaces and writer accounts to the controller", async () => {
    const { stdout } = await execa("helm", [
      "template", "kars", chart, "--namespace", "kars-system",
      "--set", "sre.enabled=true", "--show-only", "templates/sre.yaml",
    ]);
    const resources = documents(stdout);
    expect(resources.some(resource => resource.kind === "Namespace" || resource.kind === "ServiceAccount")).toBe(false);
    const namespaced = resources.filter(resource => resource.metadata.namespace);
    expect(namespaced).toHaveLength(3);
    expect(namespaced.every(resource => resource.metadata.namespace === "kars-system")).toBe(true);
    expect(resources.some(resource => resource.kind === "KarsSandbox" && resource.metadata.name === "sre")).toBe(true);
  });

  it.each([true, false])("retains a legacy namespace without deleting its data when enabled=%s", async enabled => {
    const namespace = legacy("Namespace", "kars-sre");
    const writer = legacy("ServiceAccount", "sre-writer");
    const resources = await upgrade(namespace, writer, enabled);
    const retained = resources.find(resource => resource.kind === "Namespace")!;
    expect(retained.metadata.labels).toEqual(namespace.metadata.labels);
    expect(retained.metadata.annotations).toEqual({
      ...namespace.metadata.annotations, "helm.sh/resource-policy": "keep",
    });
    const account = resources.find(resource => resource.kind === "ServiceAccount");
    if (enabled) {
      expect(account?.metadata.annotations).toEqual({
        ...writer.metadata.annotations, "kars.azure.com/no-automount": "true",
      });
      expect(account?.metadata.labels).toEqual(writer.metadata.labels);
      expect(account?.automountServiceAccountToken).toBe(false);
    } else {
      expect(account).toBeUndefined();
      expect(resources).toHaveLength(1);
    }
  });

  it("does not claim another release's namespace or service account", async () => {
    const resources = await upgrade(
      legacy("Namespace", "kars-sre", "other-release"),
      legacy("ServiceAccount", "sre-writer", "other-release"),
    );
    expect(resources.some(resource => ["Namespace", "ServiceAccount"].includes(resource.kind))).toBe(false);
  });

  it("does not invent a namespace when upgrading a release that never enabled SRE", async () => {
    const resources = await upgrade(undefined, undefined);
    expect(resources.some(resource => ["Namespace", "ServiceAccount"].includes(resource.kind))).toBe(false);
  });

  it("propagates namespace lookup failures instead of omitting a possibly owned resource", async () => {
    await expect(upgrade(undefined, undefined, true, true)).rejects.toThrow(/error calling lookup/);
  });
});
