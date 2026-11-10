// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterAll, afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { randomUUID } from "node:crypto";
import { execFileSync } from "node:child_process";
import { chmodSync, linkSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { PassThrough } from "node:stream";
import { rootCertificates } from "node:tls";
import * as path from "node:path";
import { parse } from "yaml";
import type { execa } from "execa";
import { assertFreshKindBetaCluster, installKindBeta, kindBetaAccess, kindBetaApiDestinations, kindBetaClusterConfig, kindBetaKubeletTrust, loadKindBetaPlan, probeKindBetaUI, type BetaExecute } from "./kind-beta.js";
import { prepareKindBetaInference } from "./kind-beta-inference.js";
import { setupInstalledKindBetaProviders } from "./kind-beta-providers.js";
vi.mock("./kind-beta-providers.js", () => ({ setupInstalledKindBetaProviders: vi.fn(async () => {}) }));

const certificateDirectory = path.resolve(`.onboarding-kind-cert-${randomUUID()}`);
let kubeletChain: string;
beforeAll(() => {
  mkdirSync(certificateDirectory, { mode: 0o700 });
  const openssl = (args: string[]) => execFileSync("openssl", args, { cwd: certificateDirectory, stdio: "pipe" });
  openssl(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.pem", "-days", "2", "-subj", "/CN=kind-test-ca", "-addext", "basicConstraints=critical,CA:TRUE"]);
  openssl(["req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", "leaf.key", "-out", "leaf.csr", "-subj", "/CN=chosen-control-plane"]);
  writeFileSync(path.join(certificateDirectory, "extensions"), "subjectAltName=DNS:chosen-control-plane\nbasicConstraints=critical,CA:FALSE\n");
  openssl(["x509", "-req", "-in", "leaf.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial", "-out", "leaf.pem", "-days", "1", "-extfile", "extensions"]);
  kubeletChain = ["leaf.pem", "ca.pem"].map(file => readFileSync(path.join(certificateDirectory, file), "utf8")).join("\n");
  openssl(["req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", "budget.key", "-out", "budget.csr", "-subj", "/CN=kars-inference-budget.kars-system.svc"]);
  writeFileSync(path.join(certificateDirectory, "budget-extensions"), "subjectAltName=DNS:kars-inference-budget.kars-system.svc\nbasicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\n");
  openssl(["x509", "-req", "-in", "budget.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial", "-out", "budget.pem", "-days", "1", "-extfile", "budget-extensions"]);
  openssl(["req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", "intermediate.key", "-out", "intermediate.csr", "-subj", "/CN=kind-test-intermediate"]);
  writeFileSync(path.join(certificateDirectory, "intermediate-extensions"), "basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\n");
  openssl(["x509", "-req", "-in", "intermediate.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial", "-out", "intermediate.pem", "-days", "1", "-extfile", "intermediate-extensions"]);
  openssl(["x509", "-req", "-in", "budget.csr", "-CA", "intermediate.pem", "-CAkey", "intermediate.key", "-CAcreateserial", "-out", "budget-intermediate.pem", "-days", "1", "-extfile", "budget-extensions"]);
});
afterAll(() => rmSync(certificateDirectory, { recursive: true, force: true }));
const kubeletNode = { metadata: { name: "chosen-control-plane", uid: "fresh-node" }, status: { addresses: [
  { type: "InternalIP", address: "172.18.0.2" }, { type: "Hostname", address: "chosen-control-plane" },
] } };
const kubeletContainer = { Name: "/chosen-control-plane", State: { Running: true },
  Config: { Labels: { "io.x-k8s.kind.cluster": "chosen", "io.x-k8s.kind.role": "control-plane" } },
  NetworkSettings: { Networks: { kind: { IPAddress: "172.18.0.2" } } } };

const directories: string[] = [];
afterEach(() => { vi.restoreAllMocks(); for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true }); });
function fixture() {
  const directory = path.resolve(`.onboarding-kind-${randomUUID()}`);
  mkdirSync(directory, { mode: 0o700 }); directories.push(directory);
  const issuer = "https://login.example.test/realm";
  const core = { headlamp: { oidc: { issuerURL: issuer, clientID: "headlamp", callbackURL: "http://localhost:4466/oidc-callback",
    clientSecret: { name: "headlamp-client", sourceName: "headlamp-client", key: "client-secret" } } },
    inferenceRouter: { azure: { openai: { credentials: { existingSecret: "provider", key: "api-key" } } } },
    inferenceBudget: { tlsSecretName: "budget-tls", caBundle: readFileSync(path.join(certificateDirectory, "ca.pem"), "utf8") } };
  const bridge = { headlamp: { url: "http://localhost:4466" }, auth: { principalSecretName: "bridge-principal", principalSecretKey: "session-secret" },
    web: { extraEnv: [
      { name: "BRIDGE_OIDC_ISSUER", value: issuer }, { name: "BRIDGE_OIDC_CLIENT_ID", value: "bridge" },
      { name: "BRIDGE_OIDC_REDIRECT_URI", value: "http://localhost:3000/auth/callback" },
      { name: "BRIDGE_OIDC_ROLE_CLAIM", value: "groups" }, { name: "BRIDGE_OIDC_ROLE_MAP", value: '{"developers":"user"}' },
      { name: "BRIDGE_OIDC_CLIENT_SECRET", valueFrom: { secretKeyRef: { name: "bridge-client", key: "client-secret" } } },
    ] } };
  const secret = (name: string, data: Record<string, string>) => ({ apiVersion: "v1", kind: "Secret", metadata: { name, namespace: "kars-system" }, stringData: data });
  const resources: any[] = [secret("provider", { "api-key": "private-provider-key" }), secret("headlamp-client", { "client-secret": "private-headlamp-secret" }),
    secret("bridge-client", { "client-secret": "private-bridge-secret" }), secret("budget-tls", { "tls.crt": readFileSync(path.join(certificateDirectory, "budget.pem"), "utf8"), "tls.key": readFileSync(path.join(certificateDirectory, "budget.key"), "utf8") }),
    { apiVersion: "rbac.authorization.k8s.io/v1", kind: "ClusterRoleBinding", metadata: { name: "kind-beta-readers" },
      subjects: [{ kind: "Group", name: "oidc:developers", apiGroup: "rbac.authorization.k8s.io" }],
      roleRef: { kind: "ClusterRole", apiGroup: "rbac.authorization.k8s.io", name: "view" } }];
  const budgetTls = resources.find(resource => resource.metadata.name === "budget-tls");
  budgetTls.type = "kubernetes.io/tls";
  budgetTls.metadata.annotations = { "kars.azure.com/inference-budget-tls": "v1" };
  const config = { version: 1, nodeImage: `kindest/node:v1.35.5@sha256:${"a".repeat(64)}`, ciliumVersion: "1.19.0",
    coreValues: "core.yaml", bridgeValues: "bridge.yaml", resources: ["resources.yaml"],
    oidc: { usernameClaim: "email", usernamePrefix: "oidc:", groupsClaim: "groups", groupsPrefix: "oidc:" } };
  const save = () => {
    writeFileSync(path.join(directory, "core.yaml"), JSON.stringify(core));
    writeFileSync(path.join(directory, "bridge.yaml"), JSON.stringify(bridge));
    writeFileSync(path.join(directory, "resources.yaml"), resources.map(value => JSON.stringify(value)).join("\n---\n"));
    writeFileSync(path.join(directory, "beta.yaml"), JSON.stringify(config));
  };
  save();
  return { directory, config, core, bridge, resources, save,
    load: () => { save(); return loadKindBetaPlan(path.join(directory, "beta.yaml"), "chosen", value => `/bundled/${value}`); } };
}
const service = { metadata: { name: "kubernetes", namespace: "default" }, spec: { clusterIP: "10.96.0.1", ports: [{ port: 443, protocol: "TCP" }] } };
const slices = { items: [{ metadata: { namespace: "default", labels: { "kubernetes.io/service-name": "kubernetes" } }, addressType: "IPv4",
  endpoints: [{ addresses: ["172.18.0.2"], conditions: { ready: true } }], ports: [{ port: 6443, protocol: "TCP" }] }] };
const tools = { kind: "/tools/kind", helm: "/tools/helm", kubectl: "/tools/kubectl", runtime: "/tools/docker", env: { KIND_EXPERIMENTAL_PROVIDER: "docker" } };

function installation() {
  const f = fixture(), plan = f.load(), calls: string[] = [];
  const apiServer = { name: "kube-apiserver", command: ["kube-apiserver",
    "--oidc-issuer-url=https://login.example.test/realm", "--oidc-client-id=headlamp",
    "--oidc-username-claim=email", "--oidc-username-prefix=oidc:",
    "--oidc-groups-claim=groups", "--oidc-groups-prefix=oidc:"], volumeMounts: [] as any[] };
  const apiServerPod = { spec: { containers: [apiServer], volumes: [] as any[] } };
  const hooks = { ensureCluster: vi.fn(async () => { calls.push("kind create"); }), installLocalCoreChart: vi.fn(async () => { calls.push("core install"); }) };
  const execute = vi.fn<BetaExecute>(async (tool, args) => {
    calls.push(`${path.basename(tool)} ${args.join(" ")}`);
    if (tool === tools.kind) return { stdout: "chosen-control-plane\n" };
    if (tool === tools.runtime && args[0] === "inspect") return { stdout: JSON.stringify([kubeletContainer]) };
    if (tool === tools.runtime && args[0] === "exec") return { stdout: kubeletChain };
    if (args.includes("get") && args.includes("node")) return { stdout: JSON.stringify(kubeletNode) };
    if (args.includes("component=kube-apiserver")) return { stdout: JSON.stringify({ items: [apiServerPod] }) };
    if (args[0] === "version") return { stdout: "v4.1.3+qualified" };
    if (args[0] === "template") return { stdout: JSON.stringify({ apiVersion: "v1", kind: "Namespace", metadata: { name: "kars-system" } }) };
    if (args.includes("version")) return { stdout: JSON.stringify({ serverVersion: { gitVersion: "v1.35.5" } }) };
    if (args.includes("get") && args.includes("karssreregistration")) return { stdout: JSON.stringify({ spec: {
      sandbox: { name: "reviewed-sre" }, runtimeNamespace: { name: "reviewed-sre-runtime" },
    } }) };
    if (args.includes("endpointslices")) return { stdout: JSON.stringify(slices) };
    if (args.includes("service")) return { stdout: JSON.stringify(service) };
    return { stdout: "" };
  });
  return { ...f, plan, calls, hooks, execute, apiServer, apiServerPod, install: () => installKindBeta(plan, tools, hooks, execute, f.directory) };
}

describe("reviewed Kind beta input", () => {
  it("resolves files relative to the config, both bundled charts and real signing material", () => {
    const f = fixture(), plan = f.load();
    expect(plan.coreValues).toBe(path.join(f.directory, "core.yaml"));
    expect(plan.bridgeChart).toBe("/bundled/bridge/deploy/helm/kars-bridge");
    const principal = plan.resources.find(resource => resource.metadata.name === "bridge-principal");
    expect(principal?.stringData["session-secret"]).toMatch(/^[A-Za-z0-9_-]{64}$/);
    expect(f.load().resources.at(-1)?.stringData["session-secret"]).not.toBe(principal?.stringData["session-secret"]);
  });
  it.each([false, "false", null, 0])("rejects disabled or malformed first-install schema staging: %j", enabled => {
    const f = fixture();
    Object.assign(f.core, { schemaHook: { enabled } });
    expect(f.load).toThrow(/requires schemaHook.enabled=true/);
  });
  it("accepts the Kind profile default and explicit first-install schema staging", () => {
    const f = fixture();
    expect(f.load).not.toThrow();
    Object.assign(f.core, { schemaHook: { enabled: true } });
    expect(f.load).not.toThrow();
  });
  it.each([["sre", "routerImageBaseline"], ["sre", "runtimeImageBaseline"], ["missionDispatcher", "imageBaseline"], ["metricsServer", "hostAliasesBaseline"]])("rejects upgrade-only %s.%s before cluster creation", (section, field) => {
    const f = fixture();
    for (const value of [`example.test/image:latest@sha256:${"a".repeat(64)}`, {}, "", null]) {
      Object.assign(f.core, { [section]: { [field]: value } });
      expect(f.load).toThrow(`upgrade-only ${section}.${field}`);
    }
  });
  it.each(["builtInFunding", "tokenTopUps", "unboundedTokenAllowances"])("rejects carried-over %s but accepts empty fresh funding", field => {
    const f = fixture();
    for (const entries of [[{ accountUid: "old-account", configUid: "old-catalog" }], {}, "", null]) {
      Object.assign(f.core.inferenceBudget, { [field]: entries });
      expect(f.load).toThrow(`requires inferenceBudget.${field} to be omitted or empty`);
    }
    Object.assign(f.core.inferenceBudget, { [field]: [] });
    expect(f.load).not.toThrow();
  });
  it.each(["missing type", "wrong type", "missing annotations", "missing marker", "wrong marker"])("rejects budget TLS with %s before creating a cluster", fault => {
    const f = fixture();
    const budget = f.resources.find(resource => resource.metadata.name === "budget-tls");
    if (fault === "missing type") delete budget.type;
    if (fault === "wrong type") budget.type = "Opaque";
    if (fault === "missing annotations") delete budget.metadata.annotations;
    if (fault === "missing marker") budget.metadata.annotations = { "unrelated": "v1" };
    if (fault === "wrong marker") budget.metadata.annotations["kars.azure.com/inference-budget-tls"] = "v2";
    expect(f.load).toThrow(/requires type kubernetes.io\/tls and annotation kars.azure.com\/inference-budget-tls: v1/);
  });
  it.each(["unrelated CA", "wrong key", "wrong hostname", "malformed certificate", "private CA bundle", "CA as server"])("rejects budget TLS with %s before creating a cluster", fault => {
    const f = fixture();
    const budget = f.resources.find(resource => resource.metadata.name === "budget-tls");
    if (fault === "unrelated CA") f.core.inferenceBudget.caBundle = rootCertificates[0];
    if (fault === "wrong key") budget.stringData["tls.key"] = readFileSync(path.join(certificateDirectory, "ca.key"), "utf8");
    if (fault === "wrong hostname") {
      budget.stringData["tls.crt"] = kubeletChain;
      budget.stringData["tls.key"] = readFileSync(path.join(certificateDirectory, "leaf.key"), "utf8");
    }
    if (fault === "malformed certificate") budget.stringData["tls.crt"] = "not a certificate";
    if (fault === "private CA bundle") f.core.inferenceBudget.caBundle += budget.stringData["tls.key"];
    if (fault === "CA as server") {
      budget.stringData["tls.crt"] = f.core.inferenceBudget.caBundle;
      budget.stringData["tls.key"] = readFileSync(path.join(certificateDirectory, "ca.key"), "utf8");
    }
    expect(f.load).toThrow(/Budget TLS requires a valid server certificate/);
  });
  it.each([true, false])("requires the intermediate certificate when the leaf uses one (included=%s)", included => {
    const f = fixture();
    const budget = f.resources.find(resource => resource.metadata.name === "budget-tls");
    budget.stringData["tls.crt"] = readFileSync(path.join(certificateDirectory, "budget-intermediate.pem"), "utf8")
      + (included ? readFileSync(path.join(certificateDirectory, "intermediate.pem"), "utf8") : "");
    if (included) expect(f.load).not.toThrow();
    else expect(f.load).toThrow(/Budget TLS requires a valid server certificate/);
  });
  it.each([{ enabled: false }, { enabled: true, namespace: "another-workspace" }])("rejects disabled or misplaced fresh workspace enrollment: %j", enrollment => {
    const f = fixture();
    Object.assign(f.bridge, { workspaceEnrollment: enrollment });
    expect(f.load).toThrow(/requires credential-free enrollment in kars-system/);
  });
  it("accepts explicit credential-free enrollment in the fresh workspace", () => {
    const f = fixture();
    Object.assign(f.bridge, { workspaceEnrollment: { enabled: true, namespace: "kars-system" } });
    expect(f.load).not.toThrow();
  });
  it("accepts matching encoded TLS data without changing the supplied identity", () => {
    const f = fixture();
    const budget = f.resources.find(resource => resource.metadata.name === "budget-tls");
    budget.data = Object.fromEntries(Object.entries(budget.stringData as Record<string, string>).map(([key, value]) => [key, Buffer.from(value).toString("base64")]));
    delete budget.stringData;
    expect(f.load().resources.find(resource => resource.metadata.name === "budget-tls")).toEqual(budget);
  });
  it.each(["2020-01-01", "2100-01-01"])("rejects certificates outside their validity at %s", date => {
    const f = fixture();
    vi.useFakeTimers();
    try { vi.setSystemTime(new Date(date)); expect(f.load).toThrow(/Budget TLS requires a valid server certificate/); }
    finally { vi.useRealTimers(); }
  });
  it("allows inference setup during installation without relaxing identity or budget TLS prerequisites", () => {
    const f = fixture();
    delete (f.core.inferenceRouter.azure.openai.credentials as { existingSecret?: string }).existingSecret;
    f.resources.splice(f.resources.findIndex(resource => resource.metadata.name === "provider"), 1);
    expect(f.load().core.inferenceRouter.azure.openai.credentials.existingSecret).toBeUndefined();
    const budget = f.resources.find(resource => resource.metadata.name === "budget-tls");
    delete budget.stringData["tls.key"];
    expect(f.load).toThrow(/budget-tls must contain tls.key/);
    budget.stringData["tls.key"] = readFileSync(path.join(certificateDirectory, "budget.key"), "utf8");
    f.resources.splice(f.resources.findIndex(resource => resource.metadata.name === "headlamp-client"), 1);
    expect(f.load).toThrow(/headlamp-client must contain client-secret/);
  });
  it.each(["http://login.example.test", "https://localhost", "https://login.example.test:8443", "https://user:pass@login.example.test"])("rejects unsupported issuer %s", issuer => {
    const f = fixture(); f.core.headlamp.oidc.issuerURL = issuer;
    expect(f.load).toThrow(/HTTPS/);
  });
  it.each(["system:", "-"])("rejects unsafe principal prefix %s", prefix => {
    const f = fixture(); f.config.oidc.groupsPrefix = prefix; expect(f.load).toThrow(/prefix/);
  });
  it("rejects a mismatched Bridge issuer", () => {
    const f = fixture(); f.bridge.web.extraEnv[0].value = "https://other.example.test"; expect(f.load).toThrow(/same external issuer/);
  });
  it.each(["http://0.0.0.0:3000/auth/callback", "http://localhost:80/auth/callback", "http://localhost:3000/wrong", "http://localhost:3000/auth/callback?x=1"])("rejects incompatible callback %s", callback => {
    const f = fixture(); f.bridge.web.extraEnv[2].value = callback; expect(f.load).toThrow(/loopback/);
  });
  it("rejects conflicting tunnel ports and mismatched Headlamp URL", () => {
    const f = fixture(); f.bridge.web.extraEnv[2].value = "http://localhost:4466/auth/callback"; expect(f.load).toThrow(/different ports/);
    f.bridge.web.extraEnv[2].value = "http://localhost:3000/auth/callback"; f.bridge.headlamp.url = "http://localhost:9999"; expect(f.load).toThrow(/match their callbacks/);
  });
  it("rejects missing provider and Headlamp source Secrets", () => {
    const f = fixture(); f.resources.shift(); expect(f.load).toThrow(/provider must contain/);
  });
  it.each(["cluster-admin", "system:masters", "kars-controller"])("refuses privileged role binding %s", role => {
    const f = fixture(); f.resources.at(-1).roleRef.name = role; expect(f.load).toThrow(/OIDC|prefixed/);
  });
  it("allows an explicit read-only CRD role without Secrets or wildcard grants", () => {
    const f = fixture(); f.resources.push({ apiVersion: "rbac.authorization.k8s.io/v1", kind: "ClusterRole", metadata: { name: "kind-beta-crd-reader" },
      rules: [{ apiGroups: ["kars.azure.com"], resources: ["karssandboxes", "karsteams"], verbs: ["get", "list", "watch"] }] });
    f.resources[4].roleRef.name = "kind-beta-crd-reader"; expect(f.load().resources).toHaveLength(7);
    f.resources[5].rules[0].verbs.push("patch"); expect(f.load).toThrow(/read-only/);
    f.resources[5].rules[0].verbs = ["get"]; f.resources[5].rules[0].resources = ["secrets"]; expect(f.load).toThrow(/read-only/);
  });
  it("refuses namespace bypass, arbitrary workloads and repeated resource identities", () => {
    const f = fixture(); f.resources[0].metadata.namespace = "kube-system"; expect(f.load).toThrow(/bootstrap|Bootstrap/);
    f.resources[0].metadata.namespace = "kars-system"; f.resources[0].kind = "Deployment"; expect(f.load).toThrow(/Bootstrap/);
    f.resources[0].kind = "Secret"; f.resources.push(f.resources[0]); expect(f.load).toThrow(/Duplicate/);
  });
  it("requires an immutable node and explicit Cilium chart version", () => {
    const f = fixture(); f.config.nodeImage = "kindest/node:latest"; expect(f.load).toThrow(/digest/);
    f.config.nodeImage = `kindest/node@sha256:${"a".repeat(64)}`; f.config.ciliumVersion = "latest"; expect(f.load).toThrow(/ciliumVersion/);
  });
  it("refuses divergent component CA trust instead of silently overriding it", () => {
    const f = fixture(); Object.assign(f.core.headlamp.oidc, { caConfigMap: "headlamp-only" });
    expect(f.load).toThrow(/shared API-server/);
    Object.assign(f.config.oidc, { caFile: "ca.pem" });
    writeFileSync(path.join(f.directory, "ca.pem"), rootCertificates[0]);
    expect(f.load).toThrow(/shared API-server/);
  });
  it("uses one public CA bundle for API-server and both component projections", async () => {
    const f = installation(); Object.assign(f.config.oidc, { caFile: "ca.pem" });
    writeFileSync(path.join(f.directory, "ca.pem"), rootCertificates[0]);
    const plan = f.load();
    expect(plan.resources.at(-1)?.data["ca.crt"]).toBe(rootCertificates[0]);
    f.apiServer.command.push("--oidc-ca-file=/etc/kars/oidc-ca.crt");
    f.apiServer.volumeMounts.push({ name: "kars-oidc-ca", mountPath: "/etc/kars/oidc-ca.crt", readOnly: true });
    f.apiServerPod.spec.volumes.push({ name: "kars-oidc-ca", hostPath: { path: "/etc/kars/oidc-ca.crt", type: "File" } });
    await installKindBeta(plan, tools, f.hooks, f.execute, f.directory);
    expect(readFileSync(path.join(f.directory, "oidc-ca.crt"), "utf8")).toBe(rootCertificates[0]);
    expect(parse(readFileSync(path.join(f.directory, "core-kind.yaml"), "utf8")).headlamp.oidc)
      .toEqual({ caConfigMap: "kars-kind-oidc-ca", caSourceConfigMap: "kars-kind-oidc-ca" });
    expect(parse(readFileSync(path.join(f.directory, "bridge-kind.yaml"), "utf8")).web.oidc.caConfigMap).toBe("kars-kind-oidc-ca");
  });
  it("refuses private key data supplied as an OIDC CA", () => {
    const f = fixture(); Object.assign(f.config.oidc, { caFile: "ca.pem" }); writeFileSync(path.join(f.directory, "ca.pem"), "-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----");
    expect(f.load).toThrow(/public PEM/);
  });
});

describe("additive Bridge lifecycle isolation", () => {
  it("refuses an existing Kind target without making any install/reset calls", async () => {
    const execute = vi.fn<BetaExecute>(async () => ({ stdout: "existing-core\n" }));
    await expect(assertFreshKindBetaCluster("existing-core", tools, execute)).rejects.toThrow(/install only the separate Bridge Helm release/);
    expect(execute).toHaveBeenCalledExactlyOnceWith(tools.kind, ["get", "clusters"], { env: tools.env, stdio: "pipe" });
  });
  it.each(["failed", "malformed"])("does not mistake %s discovery for absence", async failure => {
    const execute = vi.fn<BetaExecute>(async () => {
      if (failure === "failed") throw new Error("runtime unavailable");
      return { stdout: "invalid inventory response" };
    });
    await expect(assertFreshKindBetaCluster("chosen", tools, execute)).rejects.toThrow(/discovery failed; absence was not established/);
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it("accepts actual absence without operating on other clusters", async () => {
    const execute = vi.fn<BetaExecute>(async () => ({ stdout: "other-core\n" }));
    await expect(assertFreshKindBetaCluster("chosen", tools, execute)).resolves.toBeUndefined();
    expect(execute).toHaveBeenCalledTimes(1);
  });
  it("passes prompted inference only to Core and preserves distinct releases, auth identities and credentials", async () => {
    const f = installation();
    delete f.plan.core.inferenceRouter.azure.openai.credentials.existingSecret;
    f.plan.resources = f.plan.resources.filter(resource => resource.metadata.name !== "provider");
    const original = structuredClone(f.plan.resources);
    f.plan.inference = await prepareKindBetaInference(f.plan.core, f.plan.resources, {
      load: () => null, interactive: true,
      prompt: async () => ({ provider: "foundry", endpoint: "https://inference.example.test", model: "selected-model", apiKey: "new-private-key" }),
    });
    await f.install();
    const core = parse(readFileSync(path.join(f.directory, "core-kind.yaml"), "utf8"));
    const bridgeText = readFileSync(path.join(f.directory, "bridge-kind.yaml"), "utf8");
    expect(core.controller.inferenceDefaults).toEqual({ provider: "azure-openai", model: "selected-model" });
    expect(JSON.stringify(core)).not.toContain("new-private-key");
    expect(bridgeText).not.toMatch(/selected-model|inference.example|kars-kind-inference|new-private-key/);
    expect(f.plan.resources.slice(0, original.length)).toEqual(original);
    expect(f.hooks.installLocalCoreChart).toHaveBeenCalledWith(tools.helm, tools.kubectl, "chosen", "kars", f.plan.coreChart,
      expect.any(Array), [], undefined, expect.any(Object));
    const bridge = f.execute.mock.calls.filter(([, args]) => args.includes("kars-bridge") && args.includes("install"));
    expect(bridge).toHaveLength(1);
    expect(bridge[0][1]).toContain(f.plan.bridgeChart);
    expect(bridge[0][1]).not.toContain(f.plan.coreValues);
    const secretCreates = f.execute.mock.calls.filter(([, , options]) => options?.input?.includes("new-private-key"));
    expect(secretCreates).toHaveLength(1);
    expect(secretCreates[0][1]).toContain("create");
    expect(f.calls.join(" ")).not.toMatch(/new-private-key|uninstall|delete|rollback|upgrade/);
  });
});

describe("Kind creation and cold ordering", () => {
  it("accepts an existing private ignore file without replacing it", async () => {
    const f = installation(), ignore = path.join(f.directory, ".gitignore");
    writeFileSync(ignore, "*\n", { mode: 0o600, flag: "wx" });
    await f.install();
    expect(readFileSync(ignore, "utf8")).toBe("*\n");
  });
  it.each(["symlink", "dangling-symlink", "hardlink", "shared", "directory", "oversized", "wrong-content"])("rejects a %s ignore file before operating on the cluster", async fault => {
    const f = installation(), ignore = path.join(f.directory, ".gitignore"), target = path.join(f.directory, "other");
    if (fault === "directory") mkdirSync(ignore);
    else if (fault === "symlink" || fault === "dangling-symlink" || fault === "hardlink") {
      if (fault !== "dangling-symlink") writeFileSync(target, "*\n", { mode: 0o600 });
      if (fault === "hardlink") linkSync(target, ignore); else symlinkSync(target, ignore);
    } else {
      writeFileSync(ignore, fault === "oversized" ? "*\nextra" : fault === "wrong-content" ? "!\n" : "*\n", { mode: 0o600 });
      if (fault === "shared") chmodSync(ignore, 0o644);
    }
    await expect(f.install()).rejects.toThrow();
    expect(f.execute).not.toHaveBeenCalled();
    expect(f.hooks.ensureCluster).not.toHaveBeenCalled();
  });
  it.each(["missing", "wrong-audience", "duplicate", "missing-container"])("stops before workload installation when OIDC is %s", async fault => {
    const f = installation();
    if (fault === "missing") f.apiServer.command = ["kube-apiserver"];
    if (fault === "wrong-audience") f.apiServer.command = f.apiServer.command.map(arg => arg === "--oidc-client-id=headlamp" ? "--oidc-client-id=other" : arg);
    if (fault === "duplicate") f.apiServer.command.push("--oidc-client-id=headlamp");
    if (fault === "missing-container") f.apiServerPod.spec.containers = [];
    await expect(f.install()).rejects.toThrow(/did not apply the reviewed OIDC/);
    expect(f.hooks.installLocalCoreChart).not.toHaveBeenCalled();
    expect(f.calls.some(call => call.includes("install cilium") || call.includes("install kars-bridge"))).toBe(false);
  });
  it.each(["absent", "writable", "wrong-source"])("rejects an %s CA projection before installing workloads", async fault => {
    const f = installation(); f.plan.ca = rootCertificates[0];
    f.apiServer.command.push("--oidc-ca-file=/etc/kars/oidc-ca.crt");
    if (fault !== "absent") {
      f.apiServer.volumeMounts.push({ name: "kars-oidc-ca", mountPath: "/etc/kars/oidc-ca.crt", readOnly: fault !== "writable" });
      f.apiServerPod.spec.volumes.push({ name: "kars-oidc-ca", hostPath: { path: fault === "wrong-source" ? "/etc/kars/wrong.crt" : "/etc/kars/oidc-ca.crt", type: "File" } });
    }
    await expect(f.install()).rejects.toThrow(/did not apply the reviewed OIDC/);
    expect(f.hooks.installLocalCoreChart).not.toHaveBeenCalled();
  });
  it("targets Kind's v1beta3 template with map OIDC arguments and a read-only public CA", () => {
    const plan = fixture().load(), config = kindBetaClusterConfig(plan, "/workspace/ca.crt");
    expect(config.networking.disableDefaultCNI).toBe(true);
    const patch = parse(config.nodes[0].kubeadmConfigPatches[0]);
    expect(patch.apiVersion).toBe("kubeadm.k8s.io/v1beta3");
    expect(patch.apiServer.extraArgs).toEqual({
      "oidc-issuer-url": "https://login.example.test/realm", "oidc-client-id": "headlamp",
      "oidc-username-claim": "email", "oidc-username-prefix": "oidc:",
      "oidc-groups-claim": "groups", "oidc-groups-prefix": "oidc:", "oidc-ca-file": "/etc/kars/oidc-ca.crt",
    });
    expect(config.nodes[0].extraMounts?.[0].readOnly).toBe(true);
    expect(JSON.stringify(config)).not.toMatch(/insecure|skip-tls/);
  });
  it.each([false, true])("wires owned custody and the Dex prerequisite before Core (failure=%s)", async failed => {
    const f = installation();
    const attempt = installKindBeta(f.plan, tools, { ...f.hooks,
      ensureCluster: async (_kind, _cluster, _env, _runtime, cold) => { f.calls.push("kind create"); await cold?.onCreated?.(); },
      afterCluster: async () => { f.calls.push("capture custody"); },
      beforeCore: async () => { f.calls.push("install local Dex"); if (failed) throw new Error("Dex prerequisite failed"); },
    }, f.execute, f.directory);
    if (failed) await expect(attempt).rejects.toThrow("Dex prerequisite failed"); else await attempt;
    expect(f.calls.indexOf("capture custody")).toBe(f.calls.indexOf("kind create") + 1);
    expect(f.calls.indexOf("install local Dex")).toBeGreaterThan(f.calls.findIndex(call => call.includes("install cilium")));
    if (failed) expect(f.calls).not.toContain("core install");
    else expect(f.calls.indexOf("install local Dex")).toBeLessThan(f.calls.indexOf("core install"));
  });
  it("creates Cilium before prerequisites/Core, then Bridge, with dedicated kubeconfig/context and stdin-only credentials", async () => {
    const f = installation(); const result = await f.install();
    expect(readFileSync(path.join(f.directory, ".gitignore"), "utf8")).toBe("*\n");
    expect(f.calls.findIndex(call => call.includes("install cilium"))).toBeGreaterThan(f.calls.indexOf("kind create"));
    expect(f.calls.indexOf("core install")).toBeGreaterThan(f.calls.findIndex(call => call.includes("install cilium")));
    expect(f.calls.findIndex(call => call.includes("install kars-bridge"))).toBeGreaterThan(f.calls.indexOf("core install"));
    expect(setupInstalledKindBetaProviders).toHaveBeenCalledWith("chosen", result.kubeconfig,
      path.join(f.directory, "provider-setup"), f.plan.coreChart, tools, f.execute, f.hooks.installLocalCoreChart);
    expect(f.hooks.ensureCluster).toHaveBeenCalledWith(tools.kind, "chosen", tools.env, tools.runtime,
      { config: path.join(f.directory, "kind.yaml"), kubeconfig: result.kubeconfig, image: f.plan.nodeImage });
    expect(f.hooks.installLocalCoreChart).toHaveBeenCalledWith(tools.helm, tools.kubectl, "chosen", "kars", f.plan.coreChart,
      ["/bundled/deploy/helm/kars/values-generic.yaml", "/bundled/deploy/helm/kars/values-kind-beta.yaml", f.plan.coreValues, path.join(f.directory, "core-kind.yaml")], [], undefined, { kubeconfig: result.kubeconfig });
    for (const [tool, args, options] of f.execute.mock.calls) {
      if (args[0] === "version" || args[0] === "template" || tool === tools.kind || tool === tools.runtime) continue;
      expect(args.slice(0, 4)).toEqual(["--kubeconfig", result.kubeconfig, tool === tools.helm ? "--kube-context" : "--context", "kind-chosen"]);
      expect(args.join(" ")).not.toMatch(/private-|operator-key/);
      if (args.includes("create")) { expect(args).toContain("--validate=strict"); expect(options?.input).toBeTruthy(); }
    }
    const override = parse(readFileSync(path.join(f.directory, "core-kind.yaml"), "utf8"));
    expect(override.global.installationProfile).toBe("kind-beta");
    expect(override.schemaHook.apiDestinations).toEqual([{ cidr: "10.96.0.1/32", port: 443 }, { cidr: "172.18.0.2/32", port: 6443 }]);
    expect(override.datapathWitness.nodes).toEqual(["chosen-control-plane"]);
    expect(override.metricsServer).toEqual(kindBetaKubeletTrust("chosen", kubeletNode, kubeletContainer, kubeletChain));
    expect(f.calls.findIndex(call => call.includes("/var/lib/kubelet/pki/kubelet.crt"))).toBeLessThan(f.calls.indexOf("core install"));
    expect(f.calls.join(" ")).not.toMatch(/upgrade|rollback|uninstall|delete|--atomic|agentmesh-agt.yaml|grafana|token/);
  });
  it("waits for the discovered SRE consumer and authority between Core and Bridge", async () => {
    const f = installation(); await f.install();
    const phases = ["core install", "get karssreregistration canonical", "wait --for=create deployment/reviewed-sre",
      "rollout status deployment/reviewed-sre", "wait --for=condition=Ready karssreregistration/canonical", "install kars-bridge"];
    const indices = phases.map(phase => f.calls.findIndex(call => call.includes(phase)));
    expect(indices.every((index, position) => index >= 0 && (position === 0 || index > indices[position - 1]))).toBe(true);
    for (const [, args] of f.execute.mock.calls.filter(([, args]) => args.includes("deployment/reviewed-sre"))) {
      expect(args).toContain("reviewed-sre-runtime");
      expect(args).toContain("--timeout=15m");
    }
  });
  it.each(["--for=create", "deployment-rollout", "authority-ready"])("retains Core without installing Bridge when SRE %s fails", async stage => {
    const f = installation(), original = f.execute.getMockImplementation()!;
    f.execute.mockImplementation(async (...args) => {
      const command = args[1];
      if ((stage === "--for=create" && command.includes(stage))
        || (stage === "deployment-rollout" && command.includes("deployment/reviewed-sre") && command.includes("rollout"))
        || (stage === "authority-ready" && command.includes("karssreregistration/canonical"))) throw new Error("SRE readiness failed");
      return original(...args);
    });
    await expect(f.install()).rejects.toThrow("SRE readiness failed");
    expect(f.hooks.installLocalCoreChart).toHaveBeenCalledOnce();
    expect(f.calls.some(call => call.includes("install kars-bridge"))).toBe(false);
    expect(f.calls.join(" ")).not.toMatch(/rollback|delete|upgrade|uninstall/);
  });
  it.each(["cilium", "core", "bridge"])("stops on %s failure without retry, adoption or rollback", async stage => {
    const f = installation(), original = f.execute.getMockImplementation()!;
    f.execute.mockImplementation(async (...args) => {
      if (args[1].includes("install") && args[1].includes(stage === "bridge" ? "kars-bridge" : stage)) throw new Error("retained failure");
      return original(...args);
    });
    if (stage === "core") f.hooks.installLocalCoreChart.mockRejectedValue(new Error("retained failure"));
    await expect(f.install()).rejects.toThrow("retained failure");
    if (stage === "cilium") expect(f.hooks.installLocalCoreChart).not.toHaveBeenCalled();
    if (stage !== "bridge") expect(f.calls.some(call => call.includes("install kars-bridge"))).toBe(false);
    expect(f.calls.join(" ")).not.toMatch(/rollback|delete|upgrade|uninstall/);
  });
  it("refuses an unqualified Helm before creating Kind", async () => {
    const f = installation(); f.execute.mockResolvedValueOnce({ stdout: "v3.18.0" }); await expect(f.install()).rejects.toThrow(/Helm 4.1.3/);
    expect(f.hooks.ensureCluster).not.toHaveBeenCalled();
  });
  it("refuses mismatched server version before installing Cilium", async () => {
    const f = installation(), original = f.execute.getMockImplementation()!;
    f.execute.mockImplementation(async (...args) => args[1].includes("-o") && args[1].includes("version")
      ? { stdout: JSON.stringify({ serverVersion: { gitVersion: "v1.31.0" } }) } : original(...args));
    await expect(f.install()).rejects.toThrow(/Kubernetes 1.35/); expect(f.hooks.installLocalCoreChart).not.toHaveBeenCalled();
  });
  it("derives exact API destination pairs and refuses broad/missing/unready destinations", () => {
    expect(kindBetaApiDestinations(service, slices)).toHaveLength(2);
    expect(() => kindBetaApiDestinations(service, { items: [] })).toThrow(/Missing/);
    const bad = structuredClone(slices); bad.items[0].endpoints[0].addresses = ["0.0.0.0/0"];
    expect(() => kindBetaApiDestinations(service, bad)).toThrow(/exact IPv4/);
    bad.items[0].endpoints[0].addresses = ["172.18.0.2"]; bad.items[0].endpoints[0].conditions.ready = false;
    expect(() => kindBetaApiDestinations(service, bad)).toThrow(/not ready/);
  });
});

describe("fresh Kind kubelet trust", () => {
  it("uses the owned node's DNS SAN and public CA without an insecure scrape", () => {
    const trust = kindBetaKubeletTrust("chosen", kubeletNode, kubeletContainer, kubeletChain);
    expect(trust.hostAliases).toEqual([{ ip: "172.18.0.2", hostnames: ["chosen-control-plane"] }]);
    expect(trust.kubelet).toMatchObject({ insecureTLS: false, preferredAddressTypes: ["Hostname"] });
    expect(trust.kubelet.caBundle).toBe(readFileSync(path.join(certificateDirectory, "ca.pem"), "utf8"));
    expect(JSON.stringify(trust)).not.toContain("PRIVATE KEY");
  });
  it.each(["owner", "role", "container", "stopped", "address", "hostname", "uid"])("rejects mismatched %s", mismatch => {
    const node = structuredClone(kubeletNode), container = structuredClone(kubeletContainer);
    if (mismatch === "owner") container.Config.Labels["io.x-k8s.kind.cluster"] = "another";
    if (mismatch === "role") container.Config.Labels["io.x-k8s.kind.role"] = "worker";
    if (mismatch === "container") container.Name = "/another-control-plane";
    if (mismatch === "stopped") container.State.Running = false;
    if (mismatch === "address") container.NetworkSettings.Networks.kind.IPAddress = "172.18.0.3";
    if (mismatch === "hostname") node.status.addresses[1].address = "another-control-plane";
    if (mismatch === "uid") node.metadata.uid = "";
    expect(() => kindBetaKubeletTrust("chosen", node, container, kubeletChain)).toThrow(/owned/);
  });
  it("rejects stale, not-yet-valid, foreign and private certificate material", () => {
    expect(() => kindBetaKubeletTrust("chosen", kubeletNode, kubeletContainer, kubeletChain, 0)).toThrow(/validity/);
    expect(() => kindBetaKubeletTrust("chosen", kubeletNode, kubeletContainer, kubeletChain, Date.now() + 3 * 86400000)).toThrow(/validity/);
    expect(() => kindBetaKubeletTrust("chosen", kubeletNode, kubeletContainer,
      readFileSync(path.join(certificateDirectory, "leaf.pem"), "utf8") + rootCertificates[0])).toThrow(/certificate chain/);
    expect(() => kindBetaKubeletTrust("chosen", kubeletNode, kubeletContainer,
      kubeletChain + readFileSync(path.join(certificateDirectory, "leaf.key"), "utf8"))).toThrow(/public/);
  });
  it("rejects a valid chain for a different node hostname", () => {
    const node = JSON.parse(JSON.stringify(kubeletNode).replaceAll("chosen", "different"));
    const container = JSON.parse(JSON.stringify(kubeletContainer).replaceAll("chosen", "different"));
    expect(() => kindBetaKubeletTrust("different", node, container, kubeletChain)).toThrow(/hostname/);
  });
  it("does not install Core when certificate discovery fails", async () => {
    const f = installation(), original = f.execute.getMockImplementation()!;
    f.execute.mockImplementation(async (...args) => args[0] === tools.runtime && args[1][0] === "exec"
      ? { stdout: "not a certificate" } : original(...args));
    await expect(f.install()).rejects.toThrow(/public/);
    expect(f.hooks.installLocalCoreChart).not.toHaveBeenCalled();
  });
});

describe("foreground loopback UI lifecycle", () => {
  it.each([200, 302, 399])("accepts HTTP %s without following redirects or reading the body", async status => {
    const cancel = vi.fn(async () => {});
    const fetch = vi.spyOn(globalThis, "fetch").mockResolvedValue({ status, body: { cancel } } as unknown as Response);
    await expect(probeKindBetaUI(new URL("http://localhost:3000/private"))).resolves.toBeUndefined();
    expect(fetch).toHaveBeenCalledWith(new URL("http://127.0.0.1:3000"), expect.objectContaining({ redirect: "manual" }));
    expect(cancel).toHaveBeenCalledOnce();
  });
  it.each([401, 403, 500, 502, 503, 504])("retains a fixed HTTP %s category for auth and retry handling", async status => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue({ status, body: null } as Response);
    await expect(probeKindBetaUI(new URL("http://localhost:3000"))).rejects.toThrow(`Kind beta UI is not responsive (HTTP ${status})`);
  });
  it.each([199, 400, 501, 599, NaN, "untrusted-private-value", "__proto__"])("does not persist unrecognized HTTP status %s", async status => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue({ status, body: null } as unknown as Response);
    await expect(probeKindBetaUI(new URL("http://localhost:3000"))).rejects.toThrow("Kind beta UI is not responsive (unexpected HTTP status)");
  });
  function accessFixture(fail = false) {
    const plan = fixture().load(); const children: any[] = [];
    const spawn = vi.fn((_tool: string, args: string[]) => {
      let finish!: (value: any) => void;
      const child = Object.assign(new Promise(resolve => { finish = resolve; }), {
        stdout: new PassThrough(), stderr: new PassThrough(), kill: vi.fn(() => finish({ exitCode: 0 })),
        finish: (detail = "") => { child.stderr.write(detail); finish({ exitCode: 1 }); },
      });
      children.push(child);
      queueMicrotask(() => fail && children.length === 2 ? child.finish() : child.stdout.write(`Forwarding from 127.0.0.1:${args.at(-1)!.split(":")[0]} -> service\n`));
      return child;
    });
    const probe = vi.fn(async () => {});
    const open = vi.fn(async (_url: string) => {});
    const start = () => kindBetaAccess(plan, tools, { kubeconfig: "/dedicated/config", context: "kind-chosen" }, { spawn: spawn as unknown as typeof execa, probe, open });
    return { children, spawn, probe, open, start };
  }
  it("checks both UI endpoints and stops only its tunnels on SIGINT, without deleting the cluster", async () => {
    const f = accessFixture(); vi.spyOn(console, "log").mockImplementation(() => { queueMicrotask(() => process.emit("SIGINT")); });
    await f.start(); expect(f.probe).toHaveBeenCalledTimes(2); expect(f.children).toHaveLength(2);
    expect(f.open).toHaveBeenCalledExactlyOnceWith("http://localhost:3000");
    expect(f.open.mock.invocationCallOrder[0]).toBeGreaterThan(f.probe.mock.invocationCallOrder[1]);
    for (const [, args] of f.spawn.mock.calls) { expect(args).toEqual(expect.arrayContaining(["--kubeconfig", "/dedicated/config", "--context", "kind-chosen", "--address", "127.0.0.1"])); expect(args).not.toContain("delete"); }
    for (const child of f.children) expect(child.kill).toHaveBeenCalledWith("SIGTERM");
  });
  it.each(["SIGINT", "SIGTERM"])("exits cleanly after real %s with execa-owned tunnels", signal => {
    const directory = fixture().directory;
    const script = path.join(directory, "access-signal.mjs");
    writeFileSync(script, `
      import { kindBetaAccess } from ${JSON.stringify(new URL("./kind-beta.ts", import.meta.url).href)};
      import { execa } from ${JSON.stringify(import.meta.resolve("execa"))};
      await kindBetaAccess({web:new URL('http://localhost:3000'),headlamp:new URL('http://localhost:4466')},
        {kubectl:'unused',env:process.env}, {kubeconfig:'unused',context:'unused'}, {
          spawn: (_tool,args,options) => execa(process.execPath, ['-e',
            "console.log('Forwarding from 127.0.0.1:" + args.at(-1).split(':')[0] + "');setInterval(()=>{},1000)"], options),
          probe: async () => {},
          open: async () => { setImmediate(() => process.kill(process.pid, ${JSON.stringify(signal)})); }
        });
      console.log('access-cleanly-returned');
    `);
    const output = execFileSync(process.execPath, ["--import", import.meta.resolve("tsx"), script], { encoding: "utf8", timeout: 15000 });
    expect(output).toContain("access-cleanly-returned");
  });
  it("cleans up the first tunnel when the second fails", async () => {
    const f = accessFixture(true); await expect(f.start()).rejects.toThrow(/before readiness/);
    for (const child of f.children) expect(child.kill).toHaveBeenCalledWith("SIGTERM");
    expect(f.probe).not.toHaveBeenCalled();
  });
  it("does not announce access after a failed HTTP probe", async () => {
    const f = accessFixture(); f.probe.mockRejectedValue(new Error("HTTP 503")); const log = vi.spyOn(console, "log");
    await expect(f.start()).rejects.toThrow("HTTP 503"); expect(log).not.toHaveBeenCalled();
    for (const child of f.children) expect(child.kill).toHaveBeenCalledWith("SIGTERM");
  });
  it("reports an unexplained tunnel exit and cleans up the other one", async () => {
    const f = accessFixture(); vi.spyOn(console, "log").mockImplementation(() => { queueMicrotask(() => f.children[0].finish()); });
    await expect(f.start()).rejects.toThrow(/UI tunnel exited/);
    for (const child of f.children) expect(child.kill).toHaveBeenCalledWith("SIGTERM");
  });
  it.each([0, 1])("recovers only replaced tunnel %s and rechecks HTTP without reopening the browser", async index => {
    const f = accessFixture(); vi.useFakeTimers();
    f.open.mockImplementation(async () => { f.children[index].finish("error: lost connection to pod"); });
    f.probe.mockImplementation(async () => {
      if (f.probe.mock.calls.length === 3) {
        expect(f.children[1 - index].kill).not.toHaveBeenCalled();
        queueMicrotask(() => process.emit("SIGINT"));
      }
    });
    try {
      const running = f.start();
      await vi.advanceTimersByTimeAsync(1000); await running;
      expect(f.spawn).toHaveBeenCalledTimes(3);
      expect(f.spawn.mock.calls[2]).toEqual(f.spawn.mock.calls[index]);
      expect(f.probe.mock.calls[2]).toEqual(f.probe.mock.calls[index]);
      expect(f.open).toHaveBeenCalledTimes(1);
      for (const child of f.children) expect(child.kill).toHaveBeenCalledWith("SIGTERM");
    } finally { vi.useRealTimers(); }
  });
  it("retries a replacement that is not yet HTTP-ready", async () => {
    const f = accessFixture(); vi.useFakeTimers();
    f.open.mockImplementation(async () => { f.children[0].finish("unexpected EOF"); });
    f.probe.mockImplementation(async () => {
      if (f.probe.mock.calls.length === 3) throw new Error("HTTP 503");
      if (f.probe.mock.calls.length === 4) queueMicrotask(() => process.emit("SIGINT"));
    });
    try {
      const running = f.start(); await vi.advanceTimersByTimeAsync(2000); await running;
      expect(f.spawn).toHaveBeenCalledTimes(4); expect(f.children[2].kill).toHaveBeenCalledWith("SIGTERM");
    } finally { vi.useRealTimers(); }
  });
  it.each(["HTTP 401", "HTTP 403", "Forbidden", "Unauthorized", "address already in use"])("does not retry permanent recovery failure %s", async detail => {
    const f = accessFixture(); vi.useFakeTimers();
    f.open.mockImplementation(async () => { f.children[0].finish("lost connection to pod"); });
    f.probe.mockImplementation(async () => { if (f.probe.mock.calls.length === 3) throw new Error(detail); });
    try {
      const result = expect(f.start()).rejects.toThrow(detail);
      await vi.advanceTimersByTimeAsync(1000); await result;
      expect(f.spawn).toHaveBeenCalledTimes(3);
      for (const child of f.children) expect(child.kill).toHaveBeenCalledWith("SIGTERM");
    } finally { vi.useRealTimers(); }
  });
  it("bounds continuous recovery failure and retains the cluster", async () => {
    const f = accessFixture(); vi.useFakeTimers();
    f.open.mockImplementation(async () => { f.children[0].finish("lost connection to pod"); });
    f.probe.mockImplementation(async () => { if (f.probe.mock.calls.length > 2) throw new Error("HTTP 503"); });
    try {
      const result = expect(f.start()).rejects.toThrow(/recovery timed out; cluster retained/);
      await vi.advanceTimersByTimeAsync(180000); await result;
      for (const [, args] of f.spawn.mock.calls) expect(args).toContain("port-forward");
      for (const child of f.children) expect(child.kill).toHaveBeenCalledWith("SIGTERM");
    } finally { vi.useRealTimers(); }
  });
  it("cancels recovery backoff without spawning another tunnel", async () => {
    const f = accessFixture(); vi.useFakeTimers();
    f.open.mockImplementation(async () => { f.children[0].finish("lost connection to pod"); });
    try {
      const running = f.start(); await vi.advanceTimersByTimeAsync(500);
      process.emit("SIGTERM"); await running; await vi.advanceTimersByTimeAsync(1000);
      expect(f.spawn).toHaveBeenCalledTimes(2); expect(vi.getTimerCount()).toBe(0);
    } finally { vi.useRealTimers(); }
  });
});
