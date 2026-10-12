// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { parse, parseAllDocuments } from "yaml";
import { describe, expect, it } from "vitest";

const root = fileURLToPath(new URL("../../../", import.meta.url));
async function render(chart: string, args: string[] = []) {
  const { stdout } = await execa("helm", ["template", "kars", `${root}${chart}`,
    "-n", "kars-system", ...args], { stdio: "pipe" });
  return parseAllDocuments(stdout).map(document => document.toJSON()).filter(Boolean);
}
const core = (args: string[] = []) => render("deploy/helm/kars", args);
const bridge = (args: string[] = []) => render("bridge/deploy/helm/kars-bridge", args);
const oidcArgs = [
  "--set", "headlamp.oidc.enabled=true",
  "--set-string", "headlamp.oidc.issuerURL=https://identity.example.test/dex",
  "--set-string", "headlamp.oidc.clientID=kars-headlamp",
  "--set-string", "headlamp.oidc.callbackURL=https://headlamp.example.test/oidc-callback",
  "--set-string", "headlamp.oidc.clientSecret.name=headlamp-oidc",
];

describe("Bridge cross-node router API proxy", () => {
  const enable = ["--set", "networkPolicy.routerApiServerIngress=true",
    "--api-versions", "cilium.io/v2/CiliumClusterwideNetworkPolicy"];

  it("adds only API-server ingress to router ports in sandbox namespaces", async () => {
    const plain = await bridge();
    expect(plain.some(o => o.kind === "CiliumClusterwideNetworkPolicy")).toBe(false);
    const objects = await bridge(enable);
    const policy = objects.find(o => o.kind === "CiliumClusterwideNetworkPolicy");
    expect(policy.metadata.name).toBe("kars-kars-system-router-apiserver");
    expect(policy.metadata.namespace).toBeUndefined();
    expect(policy.spec).toEqual({
      endpointSelector: { matchLabels: {
        "kars.azure.com/component": "sandbox",
        "io.cilium.k8s.namespace.labels.kars.azure.com/role": "sandbox",
      } },
      ingress: [
        { fromEntities: ["kube-apiserver"],
          toPorts: [{ ports: [{ port: "8443", protocol: "TCP" }] }] },
        { fromNodes: [{ matchLabels: { "node-role.kubernetes.io/control-plane": "" } }],
          toPorts: [{ ports: [{ port: "8443", protocol: "TCP" }] }] },
      ],
    });
    expect(objects.filter(o => o.kind !== "CiliumClusterwideNetworkPolicy")).toEqual(plain);
  });

  it("requires the Cilium clusterwide API", async () => {
    await expect(bridge(["--set", "networkPolicy.routerApiServerIngress=true"]))
      .rejects.toThrow("requires the CiliumClusterwideNetworkPolicy API");
  });

  it("rejects disabled policy creation", async () => {
    await expect(bridge([...enable, "--set", "networkPolicy.create=false"]))
      .rejects.toThrow("requires networkPolicy.create");
  });

  it("rejects a string ingress flag", async () => {
    await expect(bridge(["--set-string", "networkPolicy.routerApiServerIngress=false"]))
      .rejects.toThrow("must be a boolean");
  });
});

describe("Core-managed AL3 Headlamp", () => {
  it("owns the namespace, immutable plugin runtime, service and isolated networking in Core", async () => {
    const objects = (await core()).filter(o => o.metadata?.namespace === "headlamp" || o.metadata?.name === "headlamp");
    expect(objects.map(o => o.kind).sort()).toEqual(["Deployment", "Namespace", "NetworkPolicy", "Service", "ServiceAccount"]);
    expect(objects.find(o => o.kind === "Namespace").metadata.annotations["helm.sh/resource-policy"]).toBe("keep");
    const deployment = objects.find(o => o.kind === "Deployment");
    expect(deployment.metadata.annotations["kars.azure.com/headlamp-owner"]).toBe("kars-system/kars");
    const pod = deployment.spec.template.spec;
    expect(pod.serviceAccountName).toBe("headlamp");
    expect(pod.securityContext).toEqual({
      runAsNonRoot: true, runAsUser: 10001, runAsGroup: 10001, fsGroup: 10001,
      seccompProfile: { type: "RuntimeDefault" },
    });
    expect(pod.containers[0]).toMatchObject({
      image: "ghcr.io/azure/kars-headlamp:latest", imagePullPolicy: "Always",
      args: expect.arrayContaining(["-in-cluster", "-plugins-dir=/headlamp/plugins"]),
      securityContext: { allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: { drop: ["ALL"] } },
    });
    expect(pod.initContainers).toBeUndefined();
    expect(pod.containers[0].env).toBeUndefined();
    expect(pod.volumes).toEqual([{ name: "tmp", emptyDir: { sizeLimit: "64Mi" } }]);
    expect(objects.find(o => o.kind === "Service").spec).toMatchObject({
      type: "ClusterIP", ports: [{ name: "http", port: 80, targetPort: "http" }],
    });
    const policy = objects.find(o => o.kind === "NetworkPolicy");
    expect(policy.spec.policyTypes).toEqual(["Ingress", "Egress"]);
    expect(policy.spec.ingress).toEqual([{ ports: [{ protocol: "TCP", port: 4466 }] }]);
    expect(policy.spec.egress.flatMap((rule: { ports: { port: number }[] }) => rule.ports).map((p: { port: number }) => p.port))
      .toEqual([53, 53, 443, 6443]);
  });

  it("does not grant the dashboard service account ambient cluster-admin or resource access", async () => {
    const bindings = (await core()).filter(o => ["RoleBinding", "ClusterRoleBinding"].includes(o.kind));
    expect(bindings.flatMap(o => o.subjects ?? []).some(s => s.name === "headlamp" && s.namespace === "headlamp")).toBe(false);
  });

  it("configures native per-user OIDC without copying credentials or granting dashboard RBAC", async () => {
    const plain = await core();
    const objects = await core(oidcArgs);
    const pod = objects.find(o => o.kind === "Deployment" && o.metadata.name === "headlamp").spec.template.spec;
    expect(pod.containers[0].env).toEqual([
      { name: "HEADLAMP_CONFIG_OIDC_CLIENT_ID", value: "kars-headlamp" },
      { name: "HEADLAMP_CONFIG_OIDC_CLIENT_SECRET",
        valueFrom: { secretKeyRef: { name: "headlamp-oidc", key: "client-secret" } } },
      { name: "HEADLAMP_CONFIG_OIDC_IDP_ISSUER_URL", value: "https://identity.example.test/dex" },
      { name: "HEADLAMP_CONFIG_OIDC_CALLBACK_URL", value: "https://headlamp.example.test/oidc-callback" },
      { name: "HEADLAMP_CONFIG_OIDC_SCOPES", value: "profile,email" },
      { name: "HEADLAMP_CONFIG_OIDC_USE_PKCE", value: "true" },
      { name: "HEADLAMP_CONFIG_OIDC_SKIP_TLS_VERIFY", value: "false" },
    ]);
    expect(pod.volumes).toEqual([{ name: "tmp", emptyDir: { sizeLimit: "64Mi" } }]);
    const bindings = (items: typeof objects) => items.filter(o => ["RoleBinding", "ClusterRoleBinding"].includes(o.kind));
    expect(bindings(objects)).toEqual(bindings(plain));
    expect(objects.filter(o => o.kind === "Secret" && o.metadata.namespace === "headlamp")).toEqual([]);
    expect(objects.find(o => o.kind === "NetworkPolicy" && o.metadata.namespace === "headlamp").spec)
      .toEqual(plain.find(o => o.kind === "NetworkPolicy" && o.metadata.namespace === "headlamp").spec);
  });

  it("mounts explicit private issuer trust readonly and permits its port without disabling TLS", async () => {
    const objects = await core([...oidcArgs, "--set-string",
      "headlamp.oidc.issuerURL=https://identity.example.test:5557/dex",
      "--set-string", "headlamp.oidc.caConfigMap=headlamp-oidc-ca",
      "--set-string", "headlamp.oidc.clientSecret.key=oauth-client",
      "--set-string", "headlamp.oidc.callbackURL=http://localhost:3904/oidc-callback",
      "--set-json", 'headlamp.hostAliases=[{"ip":"10.96.0.25","hostnames":["identity.example.test"]}]']);
    const pod = objects.find(o => o.kind === "Deployment" && o.metadata.name === "headlamp").spec.template.spec;
    expect(pod.hostAliases).toEqual([{ ip: "10.96.0.25", hostnames: ["identity.example.test"] }]);
    expect(pod.volumes).toContainEqual({ name: "oidc-ca", configMap: {
      name: "headlamp-oidc-ca", defaultMode: 0o444, items: [{ key: "ca.crt", path: "ca.crt" }],
    } });
    expect(pod.containers[0].volumeMounts).toContainEqual({
      name: "oidc-ca", mountPath: "/etc/headlamp/oidc", readOnly: true,
    });
    expect(pod.containers[0].env).toContainEqual({
      name: "HEADLAMP_CONFIG_OIDC_CA_FILE", value: "/etc/headlamp/oidc/ca.crt",
    });
    const policy = objects.find(o => o.kind === "NetworkPolicy" && o.metadata.namespace === "headlamp");
    expect(policy.spec.egress.flatMap((rule: { ports: { port: number }[] }) => rule.ports).map((p: { port: number }) => p.port))
      .toEqual([53, 53, 443, 6443, 5557]);
  });

  describe("Bridge Dex registration for native Headlamp OIDC", () => {
    const clientArgs = [
      "--set", "idp.enabled=true",
      "--set", "idp.headlampClient.enabled=true",
      "--set-string", "idp.issuer=https://kars.localhost:5557/dex",
      "--set-string", "idp.headlampClient.callbackURL=http://localhost:3904/oidc-callback",
      "--set-string", "idp.headlampClient.clientSecret.name=dex-headlamp-oidc",
    ];
    const tlsArgs = [...clientArgs, "--set", "idp.dex.tls.enabled=true",
      "--set-string", "idp.dex.tls.secretName=dex-server-tls"];
    const dexConfig = (objects: Awaited<ReturnType<typeof bridge>>) =>
      parse(objects.find(o => o.kind === "ConfigMap" && o.metadata.name === "dex").data["config.yaml"]);
    const dexPod = (objects: Awaited<ReturnType<typeof bridge>>) =>
      objects.find(o => o.kind === "Deployment" && o.metadata.name === "dex").spec.template.spec;

    it("preserves the HTTP-only Bridge client when optional integration is disabled", async () => {
      const objects = await bridge(["--set", "idp.enabled=true"]);
      const config = dexConfig(objects);
      expect(config.web).toEqual({ http: "0.0.0.0:5556" });
      expect(config.staticClients).toEqual([{
        id: "kars-bridge", name: "kars Bridge", secretEnv: "DEX_CLIENT_SECRET",
        redirectURIs: ["http://localhost:3000/auth/callback"],
      }]);
      const pod = dexPod(objects);
      expect(pod.securityContext.fsGroup).toBeUndefined();
      expect(pod.volumes).toEqual([{ name: "config", configMap: { name: "dex" } }]);
      expect(pod.containers[0].ports).toEqual([{ containerPort: 5556 }]);
      expect(pod.containers[0].env).toHaveLength(1);
    });

    it("uses native TLS with an existing readonly Secret and a separate confidential client", async () => {
      const objects = await bridge([...tlsArgs, "--set-string", "idp.headlampClient.clientSecret.key=oauth-client"]);
      const config = dexConfig(objects);
      expect(config.issuer).toBe("https://kars.localhost:5557/dex");
      expect(config.web).toEqual({
        http: "0.0.0.0:5556", https: "0.0.0.0:5557",
        tlsCert: "/etc/dex-tls/tls.crt", tlsKey: "/etc/dex-tls/tls.key",
      });
      expect(config.staticClients).toHaveLength(2);
      expect(config.staticClients[1]).toEqual({
        id: "kars-headlamp", name: "kars Headlamp", secretEnv: "DEX_HEADLAMP_CLIENT_SECRET",
        redirectURIs: ["http://localhost:3904/oidc-callback"],
      });
      const pod = dexPod(objects);
      expect(pod.securityContext).toMatchObject({ runAsNonRoot: true, runAsUser: 1001, fsGroup: 1001 });
      expect(pod.volumes).toContainEqual({
        name: "tls", secret: {
          secretName: "dex-server-tls", defaultMode: 0o440,
          items: [{ key: "tls.crt", path: "tls.crt" }, { key: "tls.key", path: "tls.key" }],
        }
      });
      expect(pod.containers[0].volumeMounts).toContainEqual({
        name: "tls", mountPath: "/etc/dex-tls", readOnly: true,
      });
      expect(pod.containers[0].env).toContainEqual({
        name: "DEX_HEADLAMP_CLIENT_SECRET",
        valueFrom: { secretKeyRef: { name: "dex-headlamp-oidc", key: "oauth-client" } },
      });
      expect(objects.find(o => o.kind === "Service" && o.metadata.name === "dex").spec.ports)
        .toEqual([{ name: "http", port: 5556, targetPort: 5556 }, { name: "https", port: 5557, targetPort: "https" }]);
      expect(objects.find(o => o.kind === "NetworkPolicy" && o.metadata.name === "kars-dex").spec.ingress[0].ports)
        .toEqual([{ port: 5556, protocol: "TCP" }, { port: 5557, protocol: "TCP" }]);
      expect(objects.filter(o => o.kind === "Secret").map(o => o.metadata.name))
        .not.toContain("dex-headlamp-oidc");
      expect(objects.filter(o => o.kind === "Secret").map(o => o.metadata.name))
        .not.toContain("dex-server-tls");
      expect(objects.filter(o => ["RoleBinding", "ClusterRoleBinding"].includes(o.kind))
        .flatMap(o => o.subjects ?? []).some(s => s.name === "headlamp")).toBe(false);
    });

    it("allows a trusted ingress to terminate TLS without enabling the native listener", async () => {
      const objects = await bridge(clientArgs);
      expect(dexConfig(objects).staticClients).toHaveLength(2);
      expect(dexConfig(objects).web).toEqual({ http: "0.0.0.0:5556" });
      expect(dexPod(objects).volumes).toHaveLength(1);
    });

    it("opts into API-server identity ingress on native Dex HTTPS only without rolling Dex", async () => {
      const plain = await bridge(tlsArgs);
      expect(plain.some(o => o.kind === "CiliumNetworkPolicy")).toBe(false);
      const objects = await bridge([...tlsArgs, "--set", "networkPolicy.dexApiServerIngress=true",
        "--api-versions", "cilium.io/v2/CiliumNetworkPolicy"]);
      const policy = objects.find(o => o.kind === "CiliumNetworkPolicy");
      expect(policy.metadata).toMatchObject({ name: "kars-dex-apiserver", namespace: "kars-system" });
      expect(policy.spec).toEqual({
        endpointSelector: { matchLabels: { app: "dex" } },
        ingress: [
          { fromEntities: ["kube-apiserver"],
            toPorts: [{ ports: [{ port: "5557", protocol: "TCP" }] }] },
          { fromNodes: [{ matchLabels: { "node-role.kubernetes.io/control-plane": "" } }],
            toPorts: [{ ports: [{ port: "5557", protocol: "TCP" }] }] },
        ],
      });
      expect(objects.find(o => o.kind === "Deployment" && o.metadata.name === "dex"))
        .toEqual(plain.find(o => o.kind === "Deployment" && o.metadata.name === "dex"));
      expect(objects.filter(o => o.kind === "NetworkPolicy"))
        .toEqual(plain.filter(o => o.kind === "NetworkPolicy"));
    });

    it("rejects API-server ingress without the Cilium API", async () => {
      await expect(bridge([...tlsArgs, "--set", "networkPolicy.dexApiServerIngress=true"]))
        .rejects.toThrow("requires the CiliumNetworkPolicy API");
    });

    it.each(["networkPolicy.create", "idp.enabled", "idp.dex.tls.enabled"])(
      "rejects API-server ingress when %s is disabled", async key => {
        await expect(bridge([...tlsArgs, "--set", "networkPolicy.dexApiServerIngress=true",
          "--api-versions", "cilium.io/v2/CiliumNetworkPolicy", "--set", `${key}=false`]))
          .rejects.toThrow("requires networkPolicy.create, idp.enabled and idp.dex.tls.enabled");
      });

    it("rejects a string API-server ingress flag", async () => {
      await expect(bridge([...tlsArgs, "--set-string", "networkPolicy.dexApiServerIngress=false"]))
        .rejects.toThrow("must be a boolean");
    });

    it("can serve native TLS without adding a Headlamp client or network policy", async () => {
      const objects = await bridge([...tlsArgs, "--set", "idp.headlampClient.enabled=false",
        "--set", "networkPolicy.create=false"]);
      expect(dexConfig(objects).staticClients).toHaveLength(1);
      expect(dexPod(objects).containers[0].env).toHaveLength(1);
      expect(objects.some(o => o.kind === "NetworkPolicy" && o.metadata.name === "kars-dex")).toBe(false);
    });

    it.each([
      ["idp.dex.tls.enabled", "false", "must be a boolean"],
      ["idp.headlampClient.enabled", "true", "must be a boolean"],
      ["idp.issuer", "http://localhost:3180/dex", "issuer must be HTTPS"],
      ["idp.issuer", "https://identity.example.test?issuer=other", "issuer must be HTTPS"],
      ["idp.dex.tls.secretName", "", "existing TLS Secret"],
      ["idp.headlampClient.clientID", "", "clientID is required"],
      ["idp.headlampClient.clientID", " ", "must not be blank"],
      ["idp.headlampClient.clientID", "kars-bridge", "must differ"],
      ["idp.headlampClient.clientSecret.name", "", "existing Secret"],
      ["idp.headlampClient.clientSecret.key", "", "existing Secret"],
      ["idp.headlampClient.callbackURL", "", "callbackURL is required"],
      ["idp.headlampClient.callbackURL", "http://headlamp.example.test/oidc-callback", "HTTP only on loopback"],
      ["idp.headlampClient.callbackURL", "https://headlamp.example.test/other", "HTTP only on loopback"],
      ["idp.headlampClient.callbackURL", "https://headlamp.example.test/oidc-callback#token", "HTTP only on loopback"],
    ])("rejects invalid Dex Headlamp input %s=%s", async (key, value, error) => {
      await expect(bridge([...tlsArgs, "--set-string", `${key}=${value}`])).rejects.toThrow(error);
    });
  });
  it("preserves disabled behavior and does not duplicate existing OIDC egress ports", async () => {
    const disabled = await core([...oidcArgs, "--set", "headlamp.oidc.enabled=false",
      "--set-string", "headlamp.oidc.caConfigMap=headlamp-oidc-ca"]);
    const pod = disabled.find(o => o.kind === "Deployment" && o.metadata.name === "headlamp").spec.template.spec;
    expect(pod.containers[0].env).toBeUndefined();
    expect(pod.volumes).toEqual([{ name: "tmp", emptyDir: { sizeLimit: "64Mi" } }]);
    const objects = await core([...oidcArgs, "--set-string",
      "headlamp.oidc.issuerURL=https://[::1]:6443/dex",
      "--set-string", "headlamp.oidc.callbackURL=http://[::1]:3904/oidc-callback"]);
    const policy = objects.find(o => o.kind === "NetworkPolicy" && o.metadata.namespace === "headlamp");
    expect(policy.spec.egress.flatMap((rule: { ports: { port: number }[] }) => rule.ports).map((p: { port: number }) => p.port))
      .toEqual([53, 53, 443, 6443]);
  });

  it.each([
    ["headlamp.oidc.enabled", "false", "must be a boolean"],
    ["headlamp.oidc.issuerURL", "", "issuerURL is required"],
    ["headlamp.oidc.issuerURL", "http://localhost:3180/dex", "HTTPS issuer"],
    ["headlamp.oidc.issuerURL", "https://user:password@identity.example.test", "HTTPS issuer"],
    ["headlamp.oidc.issuerURL", "https://identity.example.test?issuer=other", "HTTPS issuer"],
    ["headlamp.oidc.issuerURL", "https://identity.example.test:65536", "port must"],
    ["headlamp.oidc.issuerURL", "https://identity.example.test:0", "port must"],
    ["headlamp.oidc.clientID", " ", "must not be blank"],
    ["headlamp.oidc.clientSecret.name", "", "existing Secret"],
    ["headlamp.oidc.clientSecret.key", "", "existing Secret"],
    ["headlamp.oidc.callbackURL", "http://headlamp.example.test/oidc-callback", "HTTP only on loopback"],
    ["headlamp.oidc.callbackURL", "https://headlamp.example.test/other", "HTTP only on loopback"],
    ["headlamp.oidc.callbackURL", "https://headlamp.example.test/oidc-callback#token", "HTTP only on loopback"],
  ])("rejects invalid OIDC input %s=%s", async (key, value, error) => {
    await expect(core([...oidcArgs, "--set-string", `${key}=${value}`])).rejects.toThrow(error);
  });

  it("uses the required local image cache in Kind instead of pulling an unpublished or upstream image", async () => {
    const objects = await core(["-f", `${root}deploy/helm/kars/values-local-dev.yaml`]);
    const pod = objects.find(o => o.kind === "Deployment" && o.metadata.name === "headlamp").spec.template.spec;
    expect(pod.containers[0]).toMatchObject({ image: "kars-headlamp:latest", imagePullPolicy: "Never" });
  });

  it("uses real Helm lookups to refuse adoption in both Helm and template installations", async () => {
    const owners: Partial<Record<"Namespace" | "Deployment", Record<string, string>>> = {};
    const requests: string[] = [];
    const groups: Record<string, Array<{ name: string; kind: string; namespaced: boolean }>> = {
      v1: [{ name: "namespaces", kind: "Namespace", namespaced: false },
        { name: "serviceaccounts", kind: "ServiceAccount", namespaced: true },
        { name: "services", kind: "Service", namespaced: true }],
      "apps/v1": [{ name: "deployments", kind: "Deployment", namespaced: true }],
      "networking.k8s.io/v1": [{ name: "networkpolicies", kind: "NetworkPolicy", namespaced: true }],
    };
    const server = createServer((request, response) => {
      requests.push(`${request.method} ${request.url}`);
      const path = new URL(request.url!, "http://fixture.invalid").pathname;
      const groupVersion = path === "/api/v1" ? "v1" : path.replace(/^\/apis\//, "");
      const resources = groups[groupVersion];
      let body: unknown;
      if (path === "/version") body = { major: "1", minor: "35", gitVersion: "v1.35.0" };
      if (path === "/api") body = { apiVersion: "v1", kind: "APIVersions", versions: ["v1"] };
      if (path === "/apis") body = { apiVersion: "v1", kind: "APIGroupList",
        groups: Object.keys(groups).filter(key => key !== "v1").map(key => ({
          name: key.split("/")[0], versions: [{ groupVersion: key, version: "v1" }],
          preferredVersion: { groupVersion: key, version: "v1" },
        })) };
      if (resources) body = { apiVersion: "v1", kind: "APIResourceList",
        groupVersion, resources: resources.map(r => ({ ...r, verbs: ["get"] })) };
      const kind = path === "/api/v1/namespaces/headlamp" ? "Namespace"
        : path === "/apis/apps/v1/namespaces/headlamp/deployments/headlamp" ? "Deployment" : undefined;
      if (kind && owners[kind]) body = { apiVersion: kind === "Namespace" ? "v1" : "apps/v1", kind,
        metadata: { name: "headlamp", annotations: owners[kind] } };
      if (body === undefined) {
        response.statusCode = 404;
        body = { apiVersion: "v1", kind: "Status", reason: "NotFound", code: 404 };
      }
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify(body));
    });
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const directory = mkdtempSync(join(tmpdir(), "kars-headlamp-chart-"));
    try {
      const address = server.address();
      if (!address || typeof address === "string") throw new Error("Fixture did not listen");
      mkdirSync(join(directory, "templates"));
      for (const file of ["values.yaml", "templates/headlamp.yaml"]) {
        copyFileSync(`${root}deploy/helm/kars/${file}`, join(directory, file));
      }
      writeFileSync(join(directory, "Chart.yaml"), JSON.stringify({
        apiVersion: "v2", name: "headlamp-ownership-test", version: "0.1.0",
      }));
      const kubeconfig = join(directory, "config");
      writeFileSync(kubeconfig, JSON.stringify({
        apiVersion: "v1", kind: "Config", "current-context": "fixture",
        clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${address.port}` } }],
        contexts: [{ name: "fixture", context: { cluster: "fixture", user: "fixture" } }],
        users: [{ name: "fixture", user: {} }],
      }), { mode: 0o600 });
      const lookup = (args: string[] = []) => execa("helm", ["template", "kars", directory,
        "-n", "kars-system", "--is-upgrade", "--dry-run=server", "--disable-openapi-validation", "--kubeconfig", kubeconfig,
        ...args], { stdio: "pipe", timeout: 20_000 });
      await lookup();
      for (const kind of ["Namespace", "Deployment"] as const) {
        owners[kind] = {};
        await expect(lookup()).rejects.toThrow("instead of adopting");
        await lookup(["--set", "headlamp.enabled=false"]);
        owners[kind] = { "kars.azure.com/headlamp-owner": "kars-system/kars" };
        await lookup();
        owners[kind]["meta.helm.sh/release-name"] = "foreign";
        await expect(lookup()).rejects.toThrow("instead of adopting");
        owners[kind] = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" };
        await lookup();
      }
      expect(requests.every(request => request.startsWith("GET "))).toBe(true);
    } finally {
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
      rmSync(directory, { recursive: true, force: true });
    }
  }, 30_000);

  it("supports external Headlamp and private immutable image inputs without adopting their namespace", async () => {
    expect((await core(["--set", "headlamp.enabled=false"])).some(o => o.metadata?.namespace === "headlamp")).toBe(false);
    const objects = await core(["--set", "headlamp.image.repository=local.example/headlamp",
      "--set-string", `headlamp.image.tag=latest@sha256:${"a".repeat(64)}`,
      "--set", "headlamp.imagePullSecrets[0].name=registry-read"]);
    const pod = objects.find(o => o.kind === "Deployment" && o.metadata.name === "headlamp").spec.template.spec;
    expect(pod.containers[0].image).toBe(`local.example/headlamp:latest@sha256:${"a".repeat(64)}`);
    expect(pod.imagePullSecrets).toEqual([{ name: "registry-read" }]);
  });

  it("wires the same operator-supplied browser URL to web and BFF, not a cluster-internal guessed link", async () => {
    const urls = (objects: Awaited<ReturnType<typeof bridge>>) => objects
      .filter(o => o.kind === "Deployment" && ["kars-bridge-web", "kars-bridge-bff"].includes(o.metadata.name))
      .flatMap(o => o.spec.template.spec.containers[0].env.filter((entry: { name: string }) => entry.name === "BRIDGE_HEADLAMP_URL"));
    expect(urls(await bridge())).toEqual([]);
    expect(urls(await bridge(["--set", "headlamp=null"]))).toEqual([]);
    expect(urls(await bridge(["--set", "headlamp.url=https://dashboard.example.test"]))).toEqual([
      { name: "BRIDGE_HEADLAMP_URL", value: "https://dashboard.example.test" },
      { name: "BRIDGE_HEADLAMP_URL", value: "https://dashboard.example.test" },
    ]);
    await expect(bridge(["--set", "headlamp.url=javascript:alert(1)"])).rejects.toThrow("browser-reachable");
    for (const component of ["web", "bff"]) {
      await expect(bridge(["--set", `headlamp.url=https://dashboard.example.test,${component}.extraEnv[0].name=BRIDGE_HEADLAMP_URL,${component}.extraEnv[0].value=https://other.example.test`]))
        .rejects.toThrow("conflicts");
    }
    expect(urls(await bridge(["--set", "web.extraEnv[0].name=BRIDGE_HEADLAMP_URL,web.extraEnv[0].value=https://existing.example.test"])))
      .toEqual([{ name: "BRIDGE_HEADLAMP_URL", value: "https://existing.example.test" }]);
  });

  it("reuses pinned upstream assets and rebuilds the existing Kars plugin without runtime installation", () => {
    const recipe = readFileSync(`${root}sandbox-images/headlamp/Dockerfile`, "utf8");
    expect(recipe).toContain("ghcr.io/headlamp-k8s/headlamp@sha256:89c6c65810bfde61796483c93c70d659104355593792bf55cab680d685da8eeb");
    expect(recipe).toContain("mcr.microsoft.com/azurelinux/distroless/base:3.0");
    expect(recipe).toContain("RUN npm ci --no-audit --no-fund");
    expect(recipe).toContain("RUN npm run build");
    expect(recipe).toContain("FROM golang:1.27.2-bookworm AS backend");
    expect(recipe).toContain("RUN sh /usr/local/bin/build-headlamp-backend /download");
    const runtime = recipe.split("FROM ${AZURELINUX_DISTROLESS}")[1];
    expect(runtime).toContain("COPY --from=plugin /build/dist /headlamp/plugins/kars");
    expect(runtime).toContain("COPY --from=plugin /build/package.json /headlamp/plugins/kars/package.json");
    expect(runtime).toContain("COPY --from=backend /download/headlamp-server /headlamp/headlamp-server");
    expect(runtime).toContain("COPY --from=frontend /build/frontend/build /headlamp/frontend");
    expect(runtime).not.toContain("COPY --from=upstream /headlamp/frontend");
    expect(recipe).toContain("node --max-old-space-size=3072 kars-build.mjs build");
    expect(runtime).not.toContain("COPY --from=upstream /headlamp /headlamp");
    expect(runtime).toContain("USER 10001:10001");
    expect(runtime).toContain("USER=headlamp");
    expect(runtime).toContain("HOME=/tmp");
    expect(runtime).not.toMatch(/^RUN /m);
    expect(runtime).not.toContain("node_modules");
    const context = readFileSync(`${root}sandbox-images/headlamp/Dockerfile.dockerignore`, "utf8");
    expect(context).toContain("!tools/headlamp-plugin/src/**");
    expect(context).toContain("!sandbox-images/headlamp/build-backend.sh");
    expect(context).toContain("!sandbox-images/headlamp/build-frontend.mjs");
    expect(context).not.toContain("!tools/headlamp-plugin/dist");
  });

  it("patches frontend dependencies without changing product identity or unrelated upstream configuration", async () => {
    const directory = mkdtempSync(join(tmpdir(), "kars-headlamp-frontend-"));
    try {
      mkdirSync(join(directory, "frontend"));
      mkdirSync(join(directory, "app"));
      writeFileSync(join(directory, "app/package.json"), JSON.stringify({ version: "0.41.0", productName: "Headlamp" }));
      const manifest = {
        dependencies: { "js-yaml": "^4.1.1", lodash: "^4.17.23", vite: "^6.4.1", react: "^18.3.1" },
        overrides: { "js-yaml": "^4.1.1", cheerio: "1.0.0-rc.12" },
        devDependencies: { vitest: "^3.0.5" },
      };
      const file = join(directory, "frontend/package.json");
      writeFileSync(file, JSON.stringify(manifest));
      const prepare = () => execa(process.execPath, [`${root}sandbox-images/headlamp/build-frontend.mjs`, "prepare"],
        { cwd: join(directory, "frontend") });
      await prepare();
      const result = JSON.parse(readFileSync(file, "utf8"));
      expect(result.dependencies).toEqual({ "js-yaml": "4.3.2", lodash: "4.18.1", vite: "6.4.3", react: "^18.3.1" });
      expect(result.devDependencies).toEqual(manifest.devDependencies);
      expect(result.overrides).toEqual({
        "js-yaml": "$js-yaml", lodash: "$lodash", cheerio: "1.0.0-rc.12",
        "fast-uri@^3.0.0": "3.1.7", "nanoid@^3.0.0": "3.3.18",
      });
      expect(readFileSync(join(directory, "frontend/.env"), "utf8")).toBe(
        "REACT_APP_HEADLAMP_VERSION=0.41.0\n" +
        "REACT_APP_HEADLAMP_GIT_VERSION=066c5be9deb14b12bcef3485b531c64a84a2a219\n" +
        "REACT_APP_HEADLAMP_PRODUCT_NAME=Headlamp\n" +
        "REACT_APP_ENABLE_REACT_QUERY_DEVTOOLS=false\n" +
        "REACT_APP_HEADLAMP_SIDEBAR_DEFAULT_OPEN=true\n");
      writeFileSync(join(directory, "app/package.json"), JSON.stringify({ version: "0.42.0", productName: "Headlamp" }));
      await expect(prepare()).rejects.toThrow();
      expect(JSON.parse(readFileSync(file, "utf8"))).toEqual(result);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  it("includes the required image in release, signing and cache workflows", () => {
    for (const [file, job, key] of [
      ["release-public-interim", "build-images", "image"],
      ["release-internal", "build-images", "image"],
      ["image-sign-sbom", "build-sign-sbom", "component"],
      ["image-cache-publish", "publish-images", "image"],
    ]) {
      const text = readFileSync(`${root}.github/workflows/${file}.yml`, "utf8");
      const document = parseAllDocuments(text)[0];
      expect(document.errors).toEqual([]);
      const workflow = document.toJSON();
      const matrix = workflow.jobs[job].strategy.matrix[key];
      expect(matrix.find((image: { name: string }) => image.name === "headlamp"))
        .toMatchObject({ dockerfile: "sandbox-images/headlamp/Dockerfile" });
      if (file.startsWith("release-")) {
        expect(workflow.jobs[job].outputs.headlamp_digest).toBe("${{ steps.digests.outputs.headlamp }}");
        expect(text).toContain("${{ needs.build-images.outputs.headlamp_digest }}");
      }
    }
  });
});
