// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createServer } from "node:http";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { describe, expect, it } from "vitest";
import { renderCoreSchemaChart } from "./core-helm-schemas.js";
import type { SchemaExecute } from "./schema-documents.js";

describe("actual Helm lookup/capabilities rendering", () => {
  it("retains the actual chart's owned SRE source using the requested server context", async () => {
    const source = { apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox",
      metadata: { name: "sre", namespace: "kars-system", uid: "live-source", resourceVersion: "19",
        annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" } },
      spec: { testFixture: "preserved-live-source" } };
    const groups: Record<string, { name: string; kind: string; namespaced: boolean }[]> = {
      "v1": [{ name: "namespaces", kind: "Namespace", namespaced: false },
        { name: "serviceaccounts", kind: "ServiceAccount", namespaced: true },
        { name: "configmaps", kind: "ConfigMap", namespaced: true },
        { name: "secrets", kind: "Secret", namespaced: true }],
      "kars.azure.com/v1alpha1": [{ name: "karssandboxes", kind: "KarsSandbox", namespaced: true },
        { name: "inferencepolicies", kind: "InferencePolicy", namespaced: true },
        { name: "toolpolicies", kind: "ToolPolicy", namespaced: true }],
      "rbac.authorization.k8s.io/v1": [{ name: "clusterrolebindings", kind: "ClusterRoleBinding", namespaced: false },
        { name: "clusterroles", kind: "ClusterRole", namespaced: false },
        { name: "rolebindings", kind: "RoleBinding", namespaced: true }],
      "networking.k8s.io/v1": [{ name: "networkpolicies", kind: "NetworkPolicy", namespaced: true }],
    };
    const requests: string[] = [];
    const server = createServer((request, response) => {
      requests.push(`${request.method} ${request.url}`);
      const path = new URL(request.url!, "http://fixture.invalid").pathname;
      response.setHeader("Content-Type", "application/json");
      let body: unknown;
      if (path === "/version") body = { major: "1", minor: "31", gitVersion: "v1.31.9" };
      if (path === "/api") body = { apiVersion: "v1", kind: "APIVersions", versions: ["v1"] };
      if (path === "/apis") body = { apiVersion: "v1", kind: "APIGroupList", groups: Object.keys(groups).filter(key => key !== "v1").map(key => {
        const [name, version] = key.split("/");
        return { name, versions: [{ groupVersion: key, version }], preferredVersion: { groupVersion: key, version } };
      }) };
      const gv = path.startsWith("/apis/") ? path.slice(6) : path === "/api/v1" ? "v1" : "";
      if (groups[gv]) body = { apiVersion: "v1", kind: "APIResourceList", groupVersion: gv,
        resources: groups[gv].map(resource => ({ ...resource, singularName: "", verbs: ["get", "list", "create", "patch"] })) };
      if (path === "/apis/kars.azure.com/v1alpha1/namespaces/kars-system/karssandboxes/sre") body = source;
      if (path === "/openapi/v3") body = { paths: Object.fromEntries(Object.keys(groups).map(group => {
        const resource = group === "v1" ? "api/v1" : `apis/${group}`;
        return [resource, { serverRelativeURL: `/openapi/v3/${resource}?hash=fixture` }];
      })) };
      if (path.startsWith("/openapi/v3/")) {
        const key = path.slice("/openapi/v3/".length).replace(/^apis\//, "").replace(/^api\//, "");
        const [group, version] = key === "v1" ? ["", "v1"] : key.split("/");
        body = { openapi: "3.0.0", info: { title: "disposable fixture", version: "v1" },
          paths: Object.fromEntries((groups[key] ?? []).map(resource => {
            const base = group ? `/apis/${group}/${version}` : `/api/${version}`;
            const route = `${base}${resource.namespaced ? "/namespaces/{namespace}" : ""}/${resource.name}`;
            return [route, { patch: { "x-kubernetes-group-version-kind": { group, version, kind: resource.kind },
              parameters: [{ name: "fieldValidation", in: "query", schema: { type: "string" } }],
              responses: { "200": { description: "fixture" } } } }];
          })),
          components: { schemas: {} } };
      }
      if (body === undefined) {
        response.statusCode = 404;
        body = { apiVersion: "v1", kind: "Status", reason: "NotFound", code: 404, message: "fixture object absent" };
      }
      response.end(JSON.stringify(body));
    });
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Fixture server did not listen");
    const directory = mkdtempSync(join(tmpdir(), "kars-schema-lookup-"));
    try {
      const chart = join(directory, "chart");
      mkdirSync(join(chart, "templates"), { recursive: true });
      const actual = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
      for (const file of ["Chart.yaml", "values.yaml", "templates/sre.yaml"]) copyFileSync(join(actual, file), join(chart, file));
      writeFileSync(join(chart, "templates/capabilities.yaml"), [
        "apiVersion: v1", "kind: ConfigMap", "metadata: {name: fixture-capabilities}", "data:",
        '  version: {{ .Capabilities.KubeVersion.Version | quote }}',
        '  upgrading: {{ .Release.IsUpgrade | quote }}',
        '  sandboxApi: {{ .Capabilities.APIVersions.Has "kars.azure.com/v1alpha1/KarsSandbox" | quote }}',
      ].join("\n"));
      const kubeconfig = join(directory, "config");
      writeFileSync(kubeconfig, JSON.stringify({
        apiVersion: "v1", kind: "Config", "current-context": "wrong-ambient",
        clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${address.port}` } }],
        contexts: [{ name: "selected", context: { cluster: "fixture", user: "operator" } },
          { name: "wrong-ambient", context: { cluster: "unavailable", user: "operator" } }],
        users: [{ name: "operator", user: {} }],
      }));
      await expect(execa("helm", ["template", "kars", chart, "-n", "kars-system", "--dry-run=client",
        "--is-upgrade", "--set", "sre.enabled=true", "--set", "sre.authorityStage=true"], { stdio: "pipe" }))
        .rejects.toThrow("Authority staging cannot CREATE");
      const execute: SchemaExecute = async (file, args, options) => {
        if (file === "helm" && args[0] === "list") return { stdout: '[{"name":"kars","namespace":"kars-system"}]' };
        if (file === "helm" && args[0] === "get" && args[1] === "values") return { stdout: '{"sre":{"enabled":true}}' };
        return execa(file, args, { ...options, env: { KUBECONFIG: kubeconfig }, timeout: 20_000 });
      };
      const rendered = await renderCoreSchemaChart(execute, ["upgrade", "kars", chart, "-n", "kars-system",
        "--kube-context", "selected", "--reset-then-reuse-values", "--set", "sre.authorityStage=true"]);
      expect(rendered.documents.find(object => object.kind === "KarsSandbox")).toEqual(source);
      expect(rendered.documents.find(object => object.metadata.name === "fixture-capabilities")?.data).toEqual({
        version: "v1.31.9", upgrading: "true", sandboxApi: "true",
      });
      expect(requests.some(request => request.endsWith("/karssandboxes/sre"))).toBe(true);
      source.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
      await expect(renderCoreSchemaChart(execute, ["upgrade", "kars", chart, "-n", "kars-system",
        "--kube-context", "selected", "--reset-then-reuse-values", "--set", "sre.authorityStage=true"]))
        .rejects.toThrow("Authority staging cannot adopt");
      expect(requests.every(request => request.startsWith("GET "))).toBe(true);
      expect(JSON.parse(await import("node:fs/promises").then(fs => fs.readFile(kubeconfig, "utf8")))["current-context"]).toBe("wrong-ambient");
    } finally {
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
      rmSync(directory, { recursive: true, force: true });
    }
  }, 30_000);
});
