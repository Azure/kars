// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { gzipSync } from "node:zlib";
import { loadYaml, type KubernetesObject, type V1Secret } from "@kubernetes/client-node";
import { describe, expect, it } from "vitest";

const exec = promisify(execFile);
const root = fileURLToPath(new URL("../../", import.meta.url));
const chart = join(root, "deploy/helm/kars-bridge");
const releaseName = "kars-bridge";
const coreNamespace = "kars-system";
const apis: Record<string, Array<[string, string, boolean]>> = {
  v1: [
    ["namespaces", "Namespace", false], ["services", "Service", true],
    ["serviceaccounts", "ServiceAccount", true], ["configmaps", "ConfigMap", true], ["secrets", "Secret", true],
  ],
  "apps/v1": [["deployments", "Deployment", true]],
  "rbac.authorization.k8s.io/v1": [
    ["roles", "Role", true], ["rolebindings", "RoleBinding", true],
    ["clusterroles", "ClusterRole", false], ["clusterrolebindings", "ClusterRoleBinding", false],
  ],
  "networking.k8s.io/v1": [["networkpolicies", "NetworkPolicy", true], ["ingresses", "Ingress", true]],
  "apiextensions.k8s.io/v1": [["customresourcedefinitions", "CustomResourceDefinition", false]],
  "kars.azure.com/v1alpha1": [
    ["karsteams", "KarsTeam", true], ["karstasks", "KarsTask", true], ["karsapprovals", "KarsApproval", true],
  ],
};
const apiPath = (version: string) => version === "v1" ? "/api/v1" : `/apis/${version}`;

function objectPath(object: KubernetesObject): string {
  const entry = apis[object.apiVersion!]?.find(([, kind]) => kind === object.kind);
  if (!entry) throw new Error(`unexpected rendered kind ${object.apiVersion}/${object.kind}`);
  return `${apiPath(object.apiVersion!)}${entry[2] ? `/namespaces/${object.metadata!.namespace}` : ""}/${entry[0]}/${object.metadata!.name}`;
}

describe("real Helm gateway removal against a loopback API fixture", () => {
  it.each(["kars-system", "bridge-private"])(
    "removes only release-owned resources, preserving core with the gateway in %s", async (namespace) => {
      const directory = join(root, `.gateway-uninstall-${randomUUID()}`);
      mkdirSync(directory);
      const objects = new Map<string, KubernetesObject>();
      const deletes: string[] = [];
      const unexpected: string[] = [];
      const releaseSecretPath = `/api/v1/namespaces/${namespace}/secrets/sh.helm.release.v1.${releaseName}.v1`;
      const api = createServer((request, response) => {
        const path = new URL(request.url!, "http://localhost").pathname;
        const reply = (code: number, body: unknown) => {
          response.writeHead(code, { "content-type": "application/json" });
          response.end(JSON.stringify(body));
        };
        if (request.method === "GET") {
          if (path === "/version") {
            reply(200, { major: "1", minor: "32", gitVersion: "v1.32.0" });
            return;
          }
          if (path === "/api") {
            reply(200, { apiVersion: "v1", kind: "APIVersions", versions: ["v1"], serverAddressByClientCIDRs: [] });
            return;
          }
          if (path === "/apis") {
            reply(200, { apiVersion: "v1", kind: "APIGroupList", groups: Object.keys(apis)
              .filter((version) => version !== "v1").map((groupVersion) => ({
                name: groupVersion.split("/")[0],
                versions: [{ groupVersion, version: groupVersion.split("/")[1] }],
                preferredVersion: { groupVersion, version: groupVersion.split("/")[1] },
              })) });
            return;
          }
          const version = Object.keys(apis).find((candidate) => apiPath(candidate) === path);
          if (version) {
            reply(200, { apiVersion: "v1", kind: "APIResourceList", groupVersion: version,
              resources: apis[version]!.map(([name, kind, namespaced]) => ({
                name, kind, namespaced, singularName: "", verbs: ["get", "list", "delete", "update"],
              })) });
            return;
          }
          if (path === `/api/v1/namespaces/${namespace}/secrets`) {
            reply(200, { apiVersion: "v1", kind: "SecretList", metadata: { resourceVersion: "1" },
              items: objects.has(releaseSecretPath) ? [objects.get(releaseSecretPath)] : [] });
            return;
          }
          if (objects.has(path)) {
            reply(200, objects.get(path));
            return;
          }
        }
        if (request.method === "PUT" && path === releaseSecretPath) {
          // Helm may encode this status-only storage update as protobuf. The
          // uninstall's resource selection/deletion uses the original manifest.
          request.resume();
          reply(200, objects.get(path));
          return;
        }
        if (request.method === "DELETE" && objects.has(path)) {
          deletes.push(path);
          objects.delete(path);
          reply(200, { apiVersion: "v1", kind: "Status", status: "Success" });
          return;
        }
        unexpected.push(`${request.method} ${path}`);
        reply(404, { apiVersion: "v1", kind: "Status", code: 404, reason: "NotFound" });
      });
      const commandOptions = {
        timeout: 20_000, maxBuffer: 4 * 1024 * 1024,
        env: { ...process.env, HOME: directory, HELM_DRIVER: "secret",
          HELM_CACHE_HOME: join(directory, "cache"), HELM_CONFIG_HOME: join(directory, "config"),
          HELM_DATA_HOME: join(directory, "data") },
      };
      try {
        const { stdout: manifest } = await exec("helm", [
          "template", releaseName, chart, "--namespace", namespace,
          "--set", `namespace=${namespace},core.namespace=${coreNamespace},createNamespace=false,teamsGateway.enabled=true`,
        ], commandOptions);
        const rendered: KubernetesObject[] = manifest.split(/^---\s*$/m)
          .filter((document) => document.split("\n").some((line) => line.trim() && !line.trimStart().startsWith("#")))
          .map((document) => loadYaml(document));
        expect(rendered.some((object) => object.kind === "Namespace")).toBe(false);
        const coreData = {
          apiVersion: "v1", kind: "ConfigMap",
          metadata: { name: "existing-core-data", namespace: coreNamespace, uid: "existing-data" },
          data: { evidence: "preserve-existing-core-data" },
        };
        const existing: KubernetesObject[] = [
          ...[...new Set([namespace, coreNamespace])].map((name) => ({
            apiVersion: "v1", kind: "Namespace", metadata: { name, uid: `existing-${name}` },
          })),
          { apiVersion: "apps/v1", kind: "Deployment", metadata: { name: "kars-controller", namespace: coreNamespace, uid: "existing-controller" } },
          coreData,
          { apiVersion: "apiextensions.k8s.io/v1", kind: "CustomResourceDefinition",
            metadata: { name: "karsteams.kars.azure.com", uid: "existing-crd" } },
          ...["KarsTeam", "KarsTask", "KarsApproval"].map((kind) => ({
            apiVersion: "kars.azure.com/v1alpha1", kind,
            metadata: { name: "existing-user-resource", namespace: coreNamespace, uid: `existing-${kind}` },
            spec: { evidence: "preserve-existing-custom-resource" },
          })),
        ];
        for (const object of existing) objects.set(objectPath(object), structuredClone(object));
        for (const object of rendered) {
          expect(objects.has(objectPath(object)), "must not adopt an existing core resource").toBe(false);
          object.metadata!.annotations = {
            ...object.metadata!.annotations,
            "meta.helm.sh/release-name": releaseName,
            "meta.helm.sh/release-namespace": namespace,
          };
          objects.set(objectPath(object), object);
        }
        // Seed Helm's persisted release record from the actual render. Uninstall
        // executes Helm's real manifest decoding, REST mapping and deletion; this
        // fixture does not claim API-server admission or live-cluster qualification.
        const release = {
          name: releaseName, namespace, version: 1, manifest, config: {},
          chart: { metadata: { name: "kars-bridge", version: "0.1.0", apiVersion: "v2" } },
          info: { status: "deployed", description: "loopback fixture",
            first_deployed: "2026-01-01T00:00:00Z", last_deployed: "2026-01-01T00:00:00Z" },
        };
        const secret: V1Secret = {
          apiVersion: "v1", kind: "Secret", type: "helm.sh/release.v1",
          metadata: { name: `sh.helm.release.v1.${releaseName}.v1`, namespace, resourceVersion: "1",
            labels: { owner: "helm", name: releaseName, status: "deployed", version: "1" } },
          data: { release: Buffer.from(gzipSync(JSON.stringify(release)).toString("base64")).toString("base64") },
        };
        objects.set(releaseSecretPath, secret);
        await new Promise<void>((resolve) => api.listen(0, "127.0.0.1", resolve));
        const address = api.address();
        if (!address || typeof address === "string") throw new Error("fixture did not bind");
        const kubeconfig = join(directory, "kubeconfig");
        writeFileSync(kubeconfig, JSON.stringify({
          apiVersion: "v1", kind: "Config",
          clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${address.port}` } }],
          users: [{ name: "fixture", user: {} }],
          contexts: [{ name: "fixture", context: { cluster: "fixture", user: "fixture" } }],
          "current-context": "fixture",
        }), { mode: 0o600 });
        const { stdout } = await exec("helm", [
          "uninstall", releaseName, "--namespace", namespace, "--kubeconfig", kubeconfig, "--no-hooks",
        ], commandOptions);
        expect(stdout).toContain(`release "${releaseName}" uninstalled`);
        expect(unexpected).toEqual([]);
        expect(deletes.sort()).toEqual([...rendered.map(objectPath), releaseSecretPath].sort());
        for (const object of existing) expect(objects.get(objectPath(object))).toEqual(object);
        expect(objects.size).toBe(existing.length);
        const coreRoleName = `${releaseName}-${namespace}-teams-gateway-core-read`;
        for (const plural of ["roles", "rolebindings"]) {
          expect(deletes).toContain(`/apis/rbac.authorization.k8s.io/v1/namespaces/${coreNamespace}/${plural}/${coreRoleName}`);
        }
        expect(deletes).toContain(`/api/v1/namespaces/${namespace}/configmaps/kars-teams-conversations`);
      } finally {
        if (api.listening) {
          api.closeAllConnections();
          await new Promise<void>((resolve, reject) => api.close((error) => error ? reject(error) : resolve()));
        }
        rmSync(directory, { recursive: true, force: true });
      }
    }, 30_000);
});
