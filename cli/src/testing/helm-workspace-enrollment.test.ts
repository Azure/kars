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

const chart = fileURLToPath(new URL("../../../bridge/deploy/helm/kars-bridge", import.meta.url));
const minimal = {
  workspaceUid: "workspace-uid", enabled: true,
  writers: [], agentKeys: [], integrationStores: [], legacyImports: [],
  observationTargets: [], githubConnections: [],
};
function owned(spec: Record<string, unknown> = minimal) {
  return {
    apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
    metadata: {
      name: "workspace", namespace: "kars-system", uid: "grant-uid",
      labels: { "app.kubernetes.io/managed-by": "Helm" },
      annotations: {
        "meta.helm.sh/release-name": "kars-bridge",
        "meta.helm.sh/release-namespace": "bridge-ui",
      },
    }, spec,
  };
}
interface Options {
  grant?: ReturnType<typeof owned>;
  namespace?: string;
  namespaceMissing?: boolean;
  terminatingNamespace?: boolean;
  forbidden?: "namespace" | "grant";
  enabled?: string;
  client?: boolean;
  upgrade?: boolean;
}
async function render(options: Options = {}) {
  const requests: string[] = [];
  const namespace = options.namespace ?? "kars-system";
  const server = createServer((request, response) => {
    const path = new URL(request.url!, "http://localhost").pathname;
    requests.push(path);
    response.setHeader("content-type", "application/json");
    let result: unknown;
    const nsPath = `/api/v1/namespaces/${namespace}`;
    const grantPath = `/apis/kars.azure.com/v1alpha1/namespaces/${namespace}/karscredentialgrants/workspace`;
    if (path === "/version") result = { major: "1", minor: "35", gitVersion: "v1.35.0" };
    if (path === "/api") result = { kind: "APIVersions", apiVersion: "v1", versions: ["v1"] };
    if (path === "/apis") result = {
      kind: "APIGroupList", apiVersion: "v1", groups: [{
        name: "kars.azure.com", versions: [{ groupVersion: "kars.azure.com/v1alpha1", version: "v1alpha1" }],
        preferredVersion: { groupVersion: "kars.azure.com/v1alpha1", version: "v1alpha1" },
      }],
    };
    if (path === "/api/v1") result = {
      kind: "APIResourceList", apiVersion: "v1", groupVersion: "v1",
      resources: [{ name: "namespaces", kind: "Namespace", namespaced: false, verbs: ["get"] }],
    };
    if (path === "/apis/kars.azure.com/v1alpha1") result = {
      kind: "APIResourceList", apiVersion: "v1", groupVersion: "kars.azure.com/v1alpha1",
      resources: [{ name: "karscredentialgrants", kind: "KarsCredentialGrant", namespaced: true, verbs: ["get"] }],
    };
    if (path === nsPath && !options.namespaceMissing) result = {
      apiVersion: "v1", kind: "Namespace", metadata: {
        name: namespace, uid: "workspace-uid",
        ...(options.terminatingNamespace ? { deletionTimestamp: "2026-10-02T00:00:00Z" } : {}),
      },
    };
    if (path === grantPath) result = options.grant;
    if ((options.forbidden === "namespace" && path === nsPath) || (options.forbidden === "grant" && path === grantPath)) {
      response.writeHead(403);
      result = { kind: "Status", apiVersion: "v1", reason: "Forbidden", code: 403, message: "lookup denied" };
    } else if (!result) {
      response.writeHead(404);
      result = { kind: "Status", apiVersion: "v1", reason: "NotFound", code: 404 };
    }
    response.end(JSON.stringify(result));
  });
  const directory = mkdtempSync(join(tmpdir(), "kars-workspace-enrollment-"));
  try {
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Expected TCP test API address");
    const target = join(directory, "chart");
    mkdirSync(join(target, "templates"), { recursive: true });
    for (const file of ["Chart.yaml", "values.yaml", "templates/_helpers.tpl", "templates/workspace-enrollment.yaml"]) {
      copyFileSync(join(chart, file), join(target, file));
    }
    const kubeconfig = join(directory, "config");
    writeFileSync(kubeconfig, JSON.stringify({
      apiVersion: "v1", kind: "Config",
      clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${address.port}` } }],
      users: [{ name: "fixture", user: {} }],
      contexts: [{ name: "fixture", context: { cluster: "fixture", user: "fixture" } }],
      "current-context": "fixture",
    }), { mode: 0o600 });
    const { stdout } = await execa("helm", [
      "template", "kars-bridge", target, "--namespace", "bridge-ui", "--set", "namespace=bridge-ui",
      "--kubeconfig", kubeconfig, `--dry-run=${options.client ? "client" : "server"}`,
      "--disable-openapi-validation",
      ...(options.upgrade ? ["--is-upgrade"] : []),
      ...(options.namespace ? ["--set", `workspaceEnrollment.namespace=${namespace}`] : []),
      ...(options.enabled === "default" ? [] : ["--set", `workspaceEnrollment.enabled=${options.enabled ?? "true"}`]),
    ], { timeout: 20_000 });
    return { resources: parseAllDocuments(stdout).map(doc => {
      if (doc.errors.length) throw doc.errors[0];
      return doc.toJSON();
    }).filter(Boolean), requests };
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    rmSync(directory, { recursive: true, force: true });
  }
}

describe("Helm credential-free workspace enrollment (real Helm lookup)", () => {
  it.each(["default", "false"])("does not create authority or look up grants when enabled=%s", async enabled => {
    const { resources, requests } = await render({ enabled, client: true });
    expect(resources).toEqual([]);
    expect(requests).toEqual([]);
  });
  it.each([false, true])("renders only UID-pinned empty authority on upgrade=%s", async upgrade => {
    const { resources } = await render({ upgrade });
    expect(resources).toHaveLength(1);
    expect(resources[0].kind).toBe("KarsCredentialGrant");
    expect(resources[0].metadata.namespace).toBe("kars-system");
    expect(resources[0].metadata.annotations["helm.sh/resource-policy"]).toBe("keep");
    expect(resources[0].spec).toEqual(minimal);
    expect(resources[0]).not.toHaveProperty("status");
  });
  it("supports another existing workspace without claiming the Bridge namespace", async () => {
    const { resources } = await render({ namespace: "team-workspace" });
    expect(resources[0].metadata.namespace).toBe("team-workspace");
    expect(resources[0].spec.workspaceUid).toBe("workspace-uid");
  });
  it("preserves the declaration on an owned minimal grant upgrade", async () => {
    const { resources } = await render({ grant: owned(), upgrade: true });
    expect(resources[0].spec).toEqual(minimal);
  });
  it.each([{ namespaceMissing: true }, { terminatingNamespace: true }, { client: true }])(
    "refuses absent/terminating namespace or offline enrollment: %j", async options => {
      await expect(render(options)).rejects.toThrow(/requires a live existing workspace namespace/);
    },
  );
  it.each(["namespace", "grant"] as const)("propagates %s lookup denial", async forbidden => {
    await expect(render({ forbidden })).rejects.toThrow(/error calling lookup/);
  });
  it("refuses an existing grant not owned by this Helm release", async () => {
    const grant = owned();
    grant.metadata.annotations["meta.helm.sh/release-name"] = "someone-else";
    await expect(render({ grant })).rejects.toThrow(/refuses to adopt/);
  });
  it.each([
    { workspaceUid: "replaced-namespace" }, { enabled: false },
  ])("refuses changed namespace identity or revocation: %j", async change => {
    await expect(render({ grant: owned({ ...minimal, ...change }) })).rejects.toThrow(/replaced workspace or disabled grant/);
  });
  it.each([
    { writers: [{ name: "writer", namespace: "kars-system", uid: "writer-uid" }] },
    { agentKeys: ["BRAVE_API_KEY"] }, { integrationStores: [{ secret: { name: "store", uid: "store-uid" } }] },
    { legacyImports: [{}] }, { observationTargets: [{}] }, { githubConnections: [{}] },
    { privateActivation: {} }, { controller: { name: "kars-controller", uid: "controller-uid" } },
    { bridgeConsumers: {} }, { futureAuthority: [] },
  ])("refuses to clear expanded authority: %j", async change => {
    await expect(render({ grant: owned({ ...minimal, ...change }) })).rejects.toThrow(/refuses to overwrite expanded grant authority/);
  });
  it("rejects a non-boolean enrollment switch", async () => {
    await expect(render({ enabled: "yes", client: true })).rejects.toThrow(/must be a boolean/);
  });
});
