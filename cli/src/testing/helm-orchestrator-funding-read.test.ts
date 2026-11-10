// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { copyFileSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";

const chart = fileURLToPath(new URL("../../../bridge/deploy/helm/kars-bridge", import.meta.url));
const core = "core-workspace";
const sourceNamespace = "actor-workspace";
const accountName = "inference-budget-source-uid";
const roleName = "bridge-access-orchestrator-funding-read";
const sourcePath = `/apis/kars.azure.com/v1alpha1/namespaces/${sourceNamespace}/karssandboxes/bridge-orchestrator`;
const accountPath = `/apis/kars.azure.com/v1alpha1/namespaces/${core}/karsbudgetaccounts/${accountName}`;
const rolePath = `/apis/rbac.authorization.k8s.io/v1/namespaces/${core}/roles/${roleName}`;
const bindingPath = rolePath.replace("/roles/", "/rolebindings/");
const rule = {
  apiGroups: ["kars.azure.com"], resources: ["karsbudgetaccounts"],
  resourceNames: [accountName], verbs: ["get"],
};
type ObjectMap = Record<string, any>;

function objects(): ObjectMap {
  const root = {
    kind: "KarsSandbox",
    resource: { namespace: sourceNamespace, name: "bridge-orchestrator", uid: "source-uid" },
    workspaceUid: "workspace-uid", clusterUid: "cluster-uid",
  };
  return {
    [sourcePath]: {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox",
      metadata: { namespace: sourceNamespace, name: "bridge-orchestrator", uid: "source-uid" },
      spec: { inferenceBudgetRef: {
        scope: "GovernedInference", root, taskUid: "source-uid", rootTaskUid: "source-uid",
        account: { namespace: core, name: accountName, uid: "account-uid" },
      } },
    },
    [accountPath]: {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsBudgetAccount",
      metadata: { namespace: core, name: accountName, uid: "account-uid",
        annotations: { "kars.azure.com/inference-budget-bootstrap": "sealed" } },
      spec: { scope: "GovernedInference", root },
    },
    [`/api/v1/namespaces/${sourceNamespace}`]: {
      apiVersion: "v1", kind: "Namespace", metadata: { name: sourceNamespace, uid: "workspace-uid" },
    },
    "/api/v1/namespaces/kube-system": {
      apiVersion: "v1", kind: "Namespace", metadata: { name: "kube-system", uid: "cluster-uid" },
    },
  };
}

function owned(kind: "Role" | "RoleBinding"): ObjectMap {
  return {
    apiVersion: "rbac.authorization.k8s.io/v1", kind,
    metadata: {
      name: roleName, namespace: core,
      labels: { "app.kubernetes.io/managed-by": "Helm" },
      annotations: { "meta.helm.sh/release-name": "bridge-access", "meta.helm.sh/release-namespace": "release-storage" },
      ownerReferences: [{ apiVersion: "kars.azure.com/v1alpha1", kind: "KarsBudgetAccount", name: accountName, uid: "account-uid" }],
    },
    ...(kind === "Role" ? { rules: [rule] } : {
      roleRef: { apiGroup: "rbac.authorization.k8s.io", kind: "Role", name: roleName },
      subjects: [{ kind: "ServiceAccount", name: "custom-bff", namespace: "bridge-private" }],
    }),
  };
}

interface Options {
  enabled?: string;
  client?: boolean;
  baseRbac?: boolean;
  missingUid?: "sandboxUid" | "accountUid";
  forbidden?: string;
  change?: (data: ObjectMap) => void;
}

async function render(options: Options = {}) {
  const data = objects();
  options.change?.(data);
  const requests: string[] = [];
  const server = createServer((request, response) => {
    const path = new URL(request.url!, "http://localhost").pathname;
    requests.push(`${request.method} ${path}`);
    response.setHeader("content-type", "application/json");
    const groups = ["kars.azure.com/v1alpha1", "rbac.authorization.k8s.io/v1"];
    const resources: ObjectMap = {
      "/api/v1": [
        { name: "namespaces", kind: "Namespace", namespaced: false, verbs: ["get"] },
        { name: "serviceaccounts", kind: "ServiceAccount", namespaced: true, verbs: ["get"] },
      ],
      "/apis/kars.azure.com/v1alpha1": [
        { name: "karssandboxes", kind: "KarsSandbox", namespaced: true, verbs: ["get"] },
        { name: "karsbudgetaccounts", kind: "KarsBudgetAccount", namespaced: true, verbs: ["get"] },
      ],
      "/apis/rbac.authorization.k8s.io/v1": [
        { name: "roles", kind: "Role", namespaced: true, verbs: ["get"] },
        { name: "rolebindings", kind: "RoleBinding", namespaced: true, verbs: ["get"] },
        { name: "clusterroles", kind: "ClusterRole", namespaced: false, verbs: ["get"] },
        { name: "clusterrolebindings", kind: "ClusterRoleBinding", namespaced: false, verbs: ["get"] },
      ],
    };
    let result = data[path];
    if (path === "/version") result = { major: "1", minor: "35", gitVersion: "v1.35.0" };
    if (path === "/api") result = { kind: "APIVersions", apiVersion: "v1", versions: ["v1"] };
    if (path === "/apis") result = {
      kind: "APIGroupList", apiVersion: "v1", groups: groups.map(group => ({
        name: group.split("/")[0], versions: [{ groupVersion: group, version: group.split("/")[1] }],
        preferredVersion: { groupVersion: group, version: group.split("/")[1] },
      })),
    };
    if (resources[path]) result = {
      kind: "APIResourceList", apiVersion: "v1",
      groupVersion: path === "/api/v1" ? "v1" : path.slice(6), resources: resources[path],
    };
    if (path === options.forbidden || !result) {
      const code = path === options.forbidden ? 403 : 404;
      response.writeHead(code);
      result = { kind: "Status", apiVersion: "v1", code, reason: code === 403 ? "Forbidden" : "NotFound", message: "lookup refused" };
    }
    response.end(JSON.stringify(result));
  });
  const directory = resolve("target", `funded-account-read-${randomUUID()}`);
  mkdirSync(join(directory, "chart", "templates"), { recursive: true });
  try {
    await new Promise<void>(done => server.listen(0, "127.0.0.1", done));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Missing local fixture address");
    for (const file of ["Chart.yaml", "values.yaml", "templates/_helpers.tpl", "templates/rbac.yaml", "templates/orchestrator-funding-read.yaml"]) {
      copyFileSync(join(chart, file), join(directory, "chart", file));
    }
    const kubeconfig = join(directory, "config");
    writeFileSync(kubeconfig, JSON.stringify({
      apiVersion: "v1", kind: "Config",
      clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${address.port}` } }],
      users: [{ name: "fixture", user: {} }],
      contexts: [{ name: "fixture", context: { cluster: "fixture", user: "fixture" } }],
      "current-context": "fixture",
    }), { mode: 0o600 });
    const selection = "rbac.fundedOrchestratorAccount";
    const { stdout } = await execa("helm", [
      "template", "bridge-access", join(directory, "chart"), "--namespace", "release-storage",
      "--kubeconfig", kubeconfig, `--dry-run=${options.client ? "client" : "server"}`,
      "--disable-openapi-validation", "--set", `namespace=bridge-private,core.namespace=${core},rbac.serviceAccountName=custom-bff,rbac.create=${options.baseRbac ?? true}`,
      "--set", `${selection}.sandboxNamespace=${sourceNamespace}`,
      ...(options.enabled === "default" ? [] : ["--set", `${selection}.enabled=${options.enabled ?? "true"}`]),
      ...(options.missingUid === "sandboxUid" ? [] : ["--set-string", `${selection}.sandboxUid=source-uid`]),
      ...(options.missingUid === "accountUid" ? [] : ["--set-string", `${selection}.accountUid=account-uid`]),
    ], { timeout: 20_000 });
    return { resources: parseAllDocuments(stdout).map(doc => {
      if (doc.errors.length) throw doc.errors[0];
      return doc.toJSON();
    }).filter(Boolean), requests };
  } finally {
    server.closeAllConnections();
    await new Promise<void>((done, reject) => server.close(error => error ? reject(error) : done()));
    rmSync(directory, { recursive: true, force: true });
  }
}

describe("Bridge exact-account funding read delegation (local Helm lookup)", () => {
  it.each(["default", "false"])("retains zero budget access by default/disabled: %s", async enabled => {
    const { resources, requests } = await render({ enabled, client: true });
    expect(requests).toEqual([]);
    const raw = parseAllDocuments(readFileSync(join(chart, "../../rbac.yaml"), "utf8")).map(doc => doc.toJSON()).filter(Boolean);
    for (const manifest of [resources, raw]) {
      expect(manifest.filter(item => item.kind.endsWith("Role")).flatMap(item => item.rules ?? [])
        .some(rule => rule.resources?.some((resource: string) => resource.startsWith("karsbudgetaccounts") || resource === "*"))).toBe(false);
    }
    const normalized = (rules: ObjectMap[]) => rules.map(rule => JSON.stringify(
      Object.fromEntries(Object.entries(rule).sort().map(([key, value]) => [key, Array.isArray(value) ? [...value].sort() : value])),
    )).sort();
    expect(normalized(resources.find(item => item.kind === "ClusterRole").rules))
      .toEqual(normalized(raw.find(item => item.kind === "ClusterRole").rules));
  });

  it.each([true, false])("delegates only one Core account GET to the configured BFF, base RBAC=%s", async baseRbac => {
    const { resources, requests } = await render({ baseRbac });
    const role = resources.find(item => item.kind === "Role");
    const binding = resources.find(item => item.kind === "RoleBinding");
    expect(role).toMatchObject(owned("Role"));
    expect(binding).toMatchObject(owned("RoleBinding"));
    expect(role.rules).toEqual([rule]);
    expect(binding.subjects).toEqual(owned("RoleBinding").subjects);
    expect(requests.every(request => request.startsWith("GET "))).toBe(true);
    expect(requests.filter(request => request.includes("/karsbudgetaccounts"))).toEqual([`GET ${accountPath}`]);
    expect(resources.some(item => item.kind === "KarsBudgetAccount" || item.kind === "KarsSandbox")).toBe(false);
  });

  it("preserves release/account-owned delegation on repeat rendering", async () => {
    const { resources } = await render({ change: data => {
      data[rolePath] = owned("Role");
      data[bindingPath] = owned("RoleBinding");
    } });
    expect(resources.find(item => item.kind === "Role").rules).toEqual([rule]);
  });

  it.each(["sandboxUid", "accountUid"] as const)("requires explicit %s", async missingUid => {
    await expect(render({ missingUid, client: true })).rejects.toThrow(/requires the explicit/);
  });
  it("refuses offline enabled rendering and non-boolean selection", async () => {
    await expect(render({ client: true })).rejects.toThrow(/live exact Sandbox UID/);
    await expect(render({ client: true, enabled: "yes" })).rejects.toThrow(/must be a boolean/);
  });
  it.each([sourcePath, accountPath, rolePath, bindingPath])("propagates unavailable live authority %s", async forbidden => {
    await expect(render({ forbidden })).rejects.toThrow(/error calling lookup/);
  });
  it.each([
    ["missing source", (data: ObjectMap) => { delete data[sourcePath]; }],
    ["replaced source", (data: ObjectMap) => { data[sourcePath].metadata.uid = "other-source"; }],
    ["terminating source", (data: ObjectMap) => { data[sourcePath].metadata.deletionTimestamp = "2026-10-01T00:00:00Z"; }],
    ["missing pin", (data: ObjectMap) => { delete data[sourcePath].spec.inferenceBudgetRef; }],
    ["changed pin", (data: ObjectMap) => { data[sourcePath].spec.inferenceBudgetRef.account.uid = "other-account"; }],
    ["missing account", (data: ObjectMap) => { delete data[accountPath]; }],
    ["replaced account", (data: ObjectMap) => { data[accountPath].metadata.uid = "other-account"; }],
    ["unsealed account", (data: ObjectMap) => { data[accountPath].metadata.annotations = {}; }],
    ["wrong root kind", (data: ObjectMap) => { data[accountPath].spec.root.kind = "KarsTask"; }],
    ["wrong source actor", (data: ObjectMap) => { data[accountPath].spec.root.resource.name = "sre"; }],
    ["replaced workspace", (data: ObjectMap) => { data[`/api/v1/namespaces/${sourceNamespace}`].metadata.uid = "other"; }],
    ["replaced cluster", (data: ObjectMap) => { data["/api/v1/namespaces/kube-system"].metadata.uid = "other"; }],
  ] as const)("rejects %s rather than extending a name-based grant", async (_, change) => {
    await expect(render({ change })).rejects.toThrow(/fundedOrchestratorAccount/);
  });
  it.each([
    ["foreign Role", rolePath, "Role", (live: ObjectMap) => { live.metadata.annotations["meta.helm.sh/release-name"] = "other"; }],
    ["foreign binding", bindingPath, "RoleBinding", (live: ObjectMap) => { live.metadata.labels = {}; }],
    ["changed account owner", rolePath, "Role", (live: ObjectMap) => { live.metadata.ownerReferences[0].uid = "old-account"; }],
    ["expanded rules", rolePath, "Role", (live: ObjectMap) => { live.rules[0] = { ...rule, verbs: ["get", "list"] }; }],
    ["changed subject", bindingPath, "RoleBinding", (live: ObjectMap) => { live.subjects[0].name = "other-bff"; }],
  ] as const)("refuses adoption/rebinding: %s", async (_, path, kind, change) => {
    await expect(render({ change: data => { data[path] = owned(kind); change(data[path]); } })).rejects.toThrow(/fundedOrchestratorAccount/);
  });
});
