// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createPrivateKey, X509Certificate } from "node:crypto";
import { createServer } from "node:http";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { parseAllDocuments } from "yaml";
import { describe, expect, it } from "vitest";

interface Resource {
  kind: string;
  metadata: { name: string; annotations?: Record<string, string> };
  data?: Record<string, string>;
  spec?: {
    caBundle?: string; insecureSkipTLSVerify?: boolean;
    service?: { name: string; namespace: string };
    template?: { spec: { containers: Array<{ image: string; args: string[]; env?: Array<{ name: string; value?: string }> }> } };
  };
}
const chart = fileURLToPath(new URL("../../../deploy/helm/kars", import.meta.url));
const parse = (yaml: string): Resource[] => parseAllDocuments(yaml).map(document => document.toJSON()).filter(Boolean);
const resource = (objects: Resource[], kind: string) => {
  const result = objects.find(object => object.kind === kind);
  if (!result) throw new Error(`Missing ${kind}`);
  return result;
};
const render = async (args: string[] = []) => parse((await execa("helm", ["template", "kars", chart,
  "-n", "kars-system", "--show-only", "templates/metrics-server.yaml", ...args], { stdio: "pipe" })).stdout);

describe("default metrics prerequisite", () => {
  it("projects the default local Hermes image and preserves an explicit override", async () => {
    for (const override of [undefined, "example.azurecr.io/kars-runtime-hermes:latest"]) {
      const { stdout } = await execa("helm", ["template", "kars", chart, "-n", "kars-system",
        "-f", join(chart, "values-local-dev.yaml"), "--show-only", "templates/controller-deployment.yaml",
        ...(override ? ["--set", `runtimes.hermes.image=${override}`] : [])], { stdio: "pipe" });
      const containers = resource(parse(stdout), "Deployment").spec!.template!.spec.containers;
      const env = containers.flatMap(container => container.env ?? []);
      expect(env.filter(entry => entry.name === "HERMES_RUNTIME_IMAGE")).toEqual([
        { name: "HERMES_RUNTIME_IMAGE", value: override ?? "kars-runtime-hermes:latest" },
      ]);
      expect(env).toContainEqual({ name: "KARS_DEV_PROFILE", value: "true" });
    }
  });

  it("renders a real AL3 image, verified serving certificate, least-privilege runtime and kubelet trust", async () => {
    const objects = await render();
    const secret = resource(objects, "Secret").data!;
    const certificate = new X509Certificate(Buffer.from(secret["tls.crt"], "base64"));
    const ca = new X509Certificate(Buffer.from(secret["ca.crt"], "base64"));
    expect(certificate.checkPrivateKey(createPrivateKey(Buffer.from(secret["tls.key"], "base64")))).toBe(true);
    expect(certificate.verify(ca.publicKey)).toBe(true);
    expect(certificate.checkHost("kars-metrics-server.kars-system.svc")).toBe("kars-metrics-server.kars-system.svc");
    expect(ca.ca).toBe(true);
    expect(Date.parse(certificate.validTo) - Date.now()).toBeGreaterThan(360 * 86400_000);
    const api = resource(objects, "APIService");
    expect(api.spec).toMatchObject({ caBundle: secret["ca.crt"], service: { name: "kars-metrics-server", namespace: "kars-system" } });
    expect(api.spec?.insecureSkipTLSVerify).not.toBe(true);
    const pod = resource(objects, "Deployment").spec!.template!.spec;
    expect(pod.containers[0].image).toBe("ghcr.io/azure/kars-metrics-server:latest");
    expect(pod.containers[0].args).toContain("--kubelet-certificate-authority=/var/run/secrets/kubernetes.io/serviceaccount/ca.crt");
    expect(pod.containers[0].args).not.toContain("--kubelet-insecure-tls");
    expect(pod.containers[0].args).toContain("--kubelet-preferred-address-types=InternalIP,ExternalIP,Hostname");
    expect(pod).toMatchObject({
      securityContext: { runAsNonRoot: true, runAsUser: 10001, seccompProfile: { type: "RuntimeDefault" } },
      containers: [{ securityContext: { readOnlyRootFilesystem: true, allowPrivilegeEscalation: false, capabilities: { drop: ["ALL"] } } }],
    });
    expect(resource(objects, "NetworkPolicy").spec).toMatchObject({
      podSelector: { matchLabels: { app: "kars-metrics-server", "kars.azure.com/metrics-managed": "true" } },
      ingress: [{ ports: [{ protocol: "TCP", port: 10250 }] }],
      egress: [{ ports: [{ protocol: "TCP", port: 443 }, { protocol: "TCP", port: 6443 }, { protocol: "TCP", port: 10250 }] },
        { ports: [{ protocol: "UDP", port: 53 }, { protocol: "TCP", port: 53 }] }],
    });
  });

  it("defaults older values to enabled and permits an explicit opt-out", async () => {
    expect(resource(await render(["--set", "metricsServer=null"]), "APIService")).toBeDefined();
    const { stdout } = await execa("helm", ["template", "kars", chart, "-n", "kars-system",
      "--set", "metricsServer.enabled=false"], { stdio: "pipe" });
    expect(parse(stdout).some(object => object.kind === "APIService" && object.metadata.name === "v1beta1.metrics.k8s.io")).toBe(false);
  });

  it("keeps managed metrics outside the broad operator network allowance", async () => {
    const { stdout } = await execa("helm", ["template", "kars", chart, "-n", "kars-system"], { stdio: "pipe" });
    const objects = parse(stdout);
    const policies = objects.filter(object => object.kind === "NetworkPolicy");
    const allowance = policies.find(object => object.metadata.name === "kars-system-operator-allow");
    expect(allowance?.spec).toMatchObject({
      podSelector: {
        matchExpressions: expect.arrayContaining([
          { key: "kars.azure.com/metrics-managed", operator: "NotIn", values: ["true"] },
        ]),
      },
    });
    const deployment = objects.find(object => object.kind === "Deployment" && object.metadata.name === "kars-metrics-server");
    expect(deployment?.spec?.template).toMatchObject({
      metadata: { labels: { "kars.azure.com/metrics-managed": "true" } },
    });
  });

  it("rejects insecure kubelet TLS outside explicit dev and retains verified aggregation in Kind", async () => {
    await expect(render(["--set", "metricsServer.kubelet.insecureTLS=true"])).rejects.toThrow("requires the explicit KARS_DEV_PROFILE");
    const objects = await render(["-f", join(chart, "values-local-dev.yaml")]);
    expect(resource(objects, "Deployment").spec!.template!.spec.containers[0].args).toContain("--kubelet-insecure-tls");
    expect(resource(objects, "APIService").spec?.insecureSkipTLSVerify).not.toBe(true);
  });

  it("mounts an explicitly configured kubelet CA", async () => {
    const objects = await render(["--set-string", "metricsServer.kubelet.caBundle=fixture-ca"]);
    expect(resource(objects, "ConfigMap").data).toEqual({ "ca.crt": "fixture-ca" });
    expect(resource(objects, "Deployment").spec!.template!.spec.containers[0].args)
      .toContain("--kubelet-certificate-authority=/var/run/kubelet-ca/ca.crt");
  });

  it("uses verified node hostnames without requiring the insecure development exception", async () => {
    const objects = await render([
      "--set-string", "metricsServer.kubelet.caBundle=fixture-ca",
      "--set-json", 'metricsServer.kubelet.preferredAddressTypes=["Hostname"]',
      "--set-json", 'metricsServer.hostAliases=[{"ip":"192.0.2.1","hostnames":["fixture-node"]}]',
    ]);
    const pod = resource(objects, "Deployment").spec!.template!.spec;
    expect(pod).toMatchObject({ hostAliases: [{ ip: "192.0.2.1", hostnames: ["fixture-node"] }] });
    expect(pod.containers[0].args).toContain("--kubelet-preferred-address-types=Hostname");
    expect(pod.containers[0].args).toContain("--kubelet-certificate-authority=/var/run/kubelet-ca/ca.crt");
    expect(pod.containers[0].args).not.toContain("--kubelet-insecure-tls");
    expect(resource(objects, "APIService").spec?.insecureSkipTLSVerify).not.toBe(true);
  });

  it("uses actual Helm lookups to retain, rotate, refuse adoption and preserve external registration", async () => {
    const baseline = await render();
    let secret: Resource | undefined = resource(baseline, "Secret");
    let api: Resource | undefined = resource(baseline, "APIService");
    const requests: string[] = [];
    const groups: Record<string, Array<{ name: string; kind: string; namespaced: boolean }>> = {
      v1: [{ name: "secrets", kind: "Secret", namespaced: true },
        { name: "serviceaccounts", kind: "ServiceAccount", namespaced: true },
        { name: "services", kind: "Service", namespaced: true }],
      "apps/v1": [{ name: "deployments", kind: "Deployment", namespaced: true }],
      "networking.k8s.io/v1": [{ name: "networkpolicies", kind: "NetworkPolicy", namespaced: true }],
      "rbac.authorization.k8s.io/v1": [{ name: "clusterroles", kind: "ClusterRole", namespaced: false },
        { name: "clusterrolebindings", kind: "ClusterRoleBinding", namespaced: false },
        { name: "rolebindings", kind: "RoleBinding", namespaced: true }],
      "apiregistration.k8s.io/v1": [{ name: "apiservices", kind: "APIService", namespaced: false }],
    };
    const server = createServer((request, response) => {
      requests.push(`${request.method} ${request.url}`);
      const path = new URL(request.url!, "http://fixture.invalid").pathname;
      let body: unknown;
      if (path === "/version") body = { major: "1", minor: "35", gitVersion: "v1.35.0" };
      if (path === "/api") body = { apiVersion: "v1", kind: "APIVersions", versions: ["v1"] };
      if (path === "/apis") body = { apiVersion: "v1", kind: "APIGroupList", groups: Object.keys(groups).filter(key => key !== "v1").map(key => {
        const [name, version] = key.split("/");
        return { name, versions: [{ groupVersion: key, version }], preferredVersion: { groupVersion: key, version } };
      }) };
      const groupVersion = path === "/api/v1" ? "v1" : path.replace(/^\/apis\//, "");
      if (groups[groupVersion]) {
        body = { apiVersion: "v1", kind: "APIResourceList",
          groupVersion, resources: groups[groupVersion].map(item => ({ ...item, verbs: ["get"] })) };
      }
      if (path.endsWith("/apiservices/v1beta1.metrics.k8s.io")) body = api;
      if (path.endsWith("/secrets/kars-metrics-server-tls")) body = secret;
      if (body === undefined) {
        response.statusCode = 404;
        body = { apiVersion: "v1", kind: "Status", reason: "NotFound", code: 404 };
      }
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify(body));
    });
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const directory = mkdtempSync(join(tmpdir(), "kars-metrics-chart-"));
    try {
      const address = server.address();
      if (!address || typeof address === "string") throw new Error("Fixture server did not listen");
      const isolatedChart = join(directory, "chart");
      mkdirSync(join(isolatedChart, "templates"), { recursive: true });
      for (const file of ["Chart.yaml", "values.yaml", "templates/metrics-server.yaml"]) {
        copyFileSync(join(chart, file), join(isolatedChart, file));
      }
      const kubeconfig = join(directory, "config");
      writeFileSync(kubeconfig, JSON.stringify({
        apiVersion: "v1", kind: "Config", "current-context": "fixture",
        clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${address.port}` } }],
        contexts: [{ name: "fixture", context: { cluster: "fixture", user: "fixture" } }],
        users: [{ name: "fixture", user: {} }],
      }), { mode: 0o600 });
      const lookup = async (args: string[] = []) => parse((await execa("helm", ["template", "kars", isolatedChart,
        "-n", "kars-system", "--is-upgrade", "--dry-run=server", "--disable-openapi-validation", "--kubeconfig", kubeconfig,
        ...args], { stdio: "pipe", timeout: 20_000 })).stdout);
      expect(resource(await lookup(), "Secret").data).toEqual(secret.data);
      const rotated = resource(await lookup(["--set-string", "metricsServer.tls.rotationRevision=renewal-1"]), "Secret");
      expect(rotated.data?.["tls.crt"]).not.toBe(secret.data?.["tls.crt"]);
      secret = rotated;
      expect(resource(await lookup(["--set-string", "metricsServer.tls.rotationRevision=renewal-1"]), "Secret").data).toEqual(rotated.data);
      secret.metadata.annotations!["meta.helm.sh/release-name"] = "foreign";
      await expect(lookup()).rejects.toThrow("refusing to adopt");
      secret = structuredClone(resource(baseline, "Secret"));
      api = { kind: "APIService", metadata: { name: "v1beta1.metrics.k8s.io" },
        spec: { service: { name: "external-metrics", namespace: "kube-system" } } };
      await expect(lookup()).rejects.toThrow("ownership changed");
      secret = undefined;
      expect(await lookup()).toEqual([]);
      api = undefined;
      expect(resource(await lookup(), "Deployment")).toBeDefined();
      expect(requests.every(request => request.startsWith("GET "))).toBe(true);
    } finally {
      await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
      rmSync(directory, { recursive: true, force: true });
    }
  }, 30_000);
});
