// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { randomUUID, X509Certificate } from "node:crypto";
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { compare } from "bcryptjs";
import { parse, parseAllDocuments, stringify } from "yaml";
import { requirePackagedAsset } from "../../lib/repo-assets.js";
import { installLocalDex, prepareLocalBeta, validateLocalBetaAccount } from "./kind-beta-local.js";
import { changeManagedHostTrust, ownedHostsContent } from "./kind-beta-host.js";
import { beginBetaState, betaDirectory, captureBetaNode, pinnedBetaTools, readBetaState, teardownManagedBeta, writeBetaState, type KindBetaState } from "./kind-beta-state.js";
import { kindBetaClusterConfig, type BetaExecute } from "./kind-beta.js";
import { prepareKindBetaInference } from "./kind-beta-inference.js";

const root = path.resolve(process.env.TMPDIR || ".", `local-dex-unit-${randomUUID()}`);
const repo = path.resolve(import.meta.dirname, "../../../..");
const owner = randomUUID(), nodeId = "a".repeat(64);
const tools = { kind: "kind", runtime: "docker", kubectl: "kubectl", helm: "helm", env: { DOCKER_CONTEXT: "foreign" } };
const execute: BetaExecute = async (tool, args) => {
  if (tool !== "openssl") throw new Error("Unit tests may execute only local OpenSSL and explicit Helm renders");
  try { return { stdout: execFileSync(tool, args, { encoding: "utf8", stdio: "pipe" }) }; }
  catch (error: any) { writeFileSync(path.join(process.env.TMPDIR || root, "local-dex-openssl-failure.log"), String(error.stderr)); throw error; }
};
beforeAll(() => mkdirSync(root, { recursive: true, mode: 0o700 }));
afterAll(() => rmSync(root, { recursive: true, force: true }));
function stateFixture() {
  const cluster = `unit-${randomUUID().slice(0, 8)}`, directory = betaDirectory(cluster, root);
  mkdirSync(directory, { mode: 0o700 });
  const state: KindBetaState = { version: 1, owner, cluster, phase: "installing", runtime: "docker", endpoint: "unix:///owned.sock", nodeId };
  writeBetaState(directory, state); return { state, directory };
}

describe("owned local beta lifecycle", () => {
  it("pins the selected endpoint, without changing the default context", async () => {
    const run = vi.fn<BetaExecute>(async () => ({ stdout: JSON.stringify([{ Endpoints: { docker: { Host: "unix:///selected.sock" } } }]) }));
    const state = await beginBetaState("unit", tools, run);
    expect(run.mock.calls[0][1]).toEqual(["context", "inspect", "foreign"]);
    expect(pinnedBetaTools(tools, state).env).toMatchObject({ DOCKER_CONTEXT: "", DOCKER_HOST: "unix:///selected.sock", KIND_EXPERIMENTAL_PROVIDER: "docker" });
    expect(tools.env.DOCKER_CONTEXT).toBe("foreign");
  });
  it("pins Podman URI and SSH identity rather than its mutable connection name", async () => {
    const state = await beginBetaState("unit", { ...tools, runtime: "podman", env: {} }, async () => ({ stdout: JSON.stringify([{ Default: true, URI: "ssh://user@127.0.0.1:5555/run/podman.sock", Identity: "/owned/key" }]) }));
    expect(pinnedBetaTools(tools, state).env).toMatchObject({ CONTAINER_CONNECTION: "", CONTAINER_HOST: state.endpoint, CONTAINER_SSHKEY: "/owned/key" });
    const probe = vi.fn<BetaExecute>();
    await expect(beginBetaState("unit", { ...tools, runtime: "podman", env: { CONTAINER_HOST: "ssh://user@remote.example/run/podman.sock" } }, probe)).rejects.toThrow(/remote host/);
    expect(probe).not.toHaveBeenCalled();
  });
  it("binds a node only with the fresh attempt's read-only CA mount", async () => {
    const { state, directory } = stateFixture();
    state.trust = { hostname: "unit.kars.test", certificate: path.join(directory, "oidc-ca.pem"), fingerprint: "test", installed: false };
    const container: any = { Id: nodeId, Config: { Labels: { "io.x-k8s.kind.cluster": state.cluster } }, Mounts: [] };
    const run: BetaExecute = async tool => ({ stdout: tool === "kind" ? `${state.cluster}-control-plane` : JSON.stringify([container]) });
    await expect(captureBetaNode(state, tools, run)).rejects.toThrow(/unique CA mount/);
    container.Mounts.push({ Source: path.join(directory, "oidc-ca.crt"), Destination: "/etc/kars/oidc-ca.crt", RW: false });
    await expect(captureBetaNode(state, tools, run)).resolves.toBeUndefined();
  });
  it("deletes only an immutable node at its original endpoint, and retains the receipt", async () => {
    const { state, directory } = stateFixture();
    const run = vi.fn<BetaExecute>(async (_tool, args) => ({ stdout: args[0] === "ps" ? nodeId : args[0] === "inspect" ? JSON.stringify([{ Id: nodeId, Config: { Labels: { "io.x-k8s.kind.cluster": state.cluster } } }]) : "" }));
    await teardownManagedBeta(directory, tools, run, vi.fn());
    expect(run.mock.calls.find(call => call[1][0] === "rm")).toEqual(["docker", ["rm", "--force", "--volumes", nodeId], { env: expect.objectContaining({ DOCKER_HOST: "unix:///owned.sock", DOCKER_CONTEXT: "" }) }]);
    expect(readBetaState(directory)?.phase).toBe("deleted");
    expect(existsSync(directory)).toBe(true);
  });
  it("safely resumes after the recorded container is already gone", async () => {
    const { directory } = stateFixture();
    const run = vi.fn<BetaExecute>(async () => ({ stdout: "" }));
    await teardownManagedBeta(directory, tools, run, vi.fn());
    expect(run).toHaveBeenCalledTimes(1);
    expect(readBetaState(directory)?.phase).toBe("deleted");
  });
  it("does not delete a changed or unrecorded node", async () => {
    const { state, directory } = stateFixture();
    const run = vi.fn<BetaExecute>(async (_tool, args) => ({ stdout: args[0] === "ps" ? nodeId : JSON.stringify([{ Id: nodeId, Config: { Labels: { "io.x-k8s.kind.cluster": "foreign" } } }]) }));
    await expect(teardownManagedBeta(directory, tools, run, vi.fn())).rejects.toThrow(/ownership changed/);
    expect(run.mock.calls.some(call => call[1][0] === "rm")).toBe(false);
    delete state.nodeId; writeBetaState(directory, state);
    await expect(teardownManagedBeta(directory, tools, run, vi.fn())).rejects.toThrow(/no verified node/);
  });
  it("refuses teardown during an active setup and rejects foreign trust paths", async () => {
    const { state, directory } = stateFixture();
    state.setupPid = process.pid; writeBetaState(directory, state);
    const run = vi.fn<BetaExecute>();
    await expect(teardownManagedBeta(directory, tools, run, vi.fn())).rejects.toThrow(/still running/);
    expect(run).not.toHaveBeenCalled();
    state.trust = { hostname: "foreign.kars.test", certificate: "/foreign/ca", fingerprint: "00", installed: true };
    writeBetaState(directory, state);
    expect(() => readBetaState(directory)).toThrow(/trust receipt/);
  });
  it("makes only a reversible marked hosts entry and rejects foreign mappings", () => {
    const original = "127.0.0.1 localhost\n# untouched\n";
    const changed = ownedHostsContent(original, "unit.kars.test", owner, true);
    expect(ownedHostsContent(changed, "unit.kars.test", owner, true)).toBe(changed);
    expect(ownedHostsContent(changed, "unit.kars.test", owner, false)).toBe(original);
    expect(() => ownedHostsContent("10.0.0.1 unit.kars.test\n", "unit.kars.test", owner, true)).toThrow(/foreign/);
  });
  it("rejects missing or escaping release assets instead of falling back to this checkout", () => {
    const bundle = path.join(root, "bundle"); mkdirSync(bundle);
    expect(() => requirePackagedAsset("deploy/helm/kars", bundle)).toThrow();
    symlinkSync(path.join(repo, "deploy"), path.join(bundle, "deploy"));
    expect(() => requirePackagedAsset("deploy/helm/kars", bundle)).toThrow();
    expect(() => requirePackagedAsset("../deploy", bundle)).toThrow();
  });
});

describe("fresh packaged Dex inputs", () => {
  it("uses real TLS, bcrypt, distinct registered clients, principal binding and no carried-over funding", async () => {
    const { directory, state } = stateFixture();
    const charts = path.join(root, "charts"); mkdirSync(charts);
    const asset = (relative: string) => path.join(charts, relative.includes("kars-bridge") ? "bridge" : "core");
    for (const name of ["deploy/helm/kars", "bridge/deploy/helm/kars-bridge"]) {
      mkdirSync(asset(name));
      const defaults = parse(readFileSync(path.join(repo, name, "values.yaml"), "utf8"));
      if (name.includes("kars-bridge")) defaults.idp.dex.image = `registry.test/idp:latest@sha256:${"b".repeat(64)}`;
      else defaults.inferenceRouter.image.tag = `latest@sha256:${"c".repeat(64)}`;
      writeFileSync(path.join(asset(name), "values.yaml"), stringify(defaults));
    }
    const account = { email: "local@example.test", password: "test-password-strong" };
    const plan = await prepareLocalBeta(directory, state, account, execute, asset);
    const dex = parse(readFileSync(path.join(directory, "dex-values.yaml"), "utf8"));
    expect(await compare(account.password, dex.idp.dex.staticPasswords[0].hash)).toBe(true);
    expect(dex.idp.dex.staticPasswords[0].groups).toBeUndefined();
    const binding = plan.resources.find(resource => resource.kind === "ClusterRoleBinding")!;
    expect(binding.subjects).toEqual([{ apiGroup: "rbac.authorization.k8s.io", kind: "User", name: "oidc:local@example.test" }]);
    const reader = plan.resources.find(resource => resource.kind === "ClusterRole")!;
    const maintainedReader = parseAllDocuments(readFileSync(path.join(repo, "deploy/headlamp-reader-rbac.yaml"), "utf8"))
      .map(document => document.toJSON()).find(resource => resource.kind === "ClusterRole");
    expect(reader.rules[0]).toEqual(maintainedReader.rules.find((rule: any) => rule.apiGroups.includes("kars.azure.com")));
    expect(reader.rules.every((rule: any) => rule.verbs.join() === "get,list,watch" && !rule.resources.includes("secrets") && !rule.resources.includes("*"))).toBe(true);
    expect(plan.core.inferenceBudget.contracts).toEqual([]);
    expect(plan.core.inferenceBudget.unboundedTokenAllowances).toBeUndefined();
    expect(new X509Certificate(plan.ca!).ca).toBe(true);
    expect(readdirSync(path.join(directory, "oidc-issuer"))).toEqual([".gitignore"]);
    expect(kindBetaClusterConfig(plan).nodes[0].extraPortMappings).toEqual([{ containerPort: 30443, hostPort: 443, listenAddress: "127.0.0.1", protocol: "TCP" }]);
    expect(kindBetaClusterConfig({ ...plan, localDex: false }).nodes[0].extraPortMappings).toBeUndefined();
    const bridge = parse(readFileSync(plan.bridgeValues, "utf8"));
    expect(bridge.web.extraEnv.find((entry: any) => entry.name === "BRIDGE_OIDC_ROLE_MAP").value).toBe('{"local@example.test":"admin"}');
    const chart = asset("bridge/deploy/helm/kars-bridge");
    cpSync(path.join(repo, "bridge/deploy/helm/kars-bridge/Chart.yaml"), path.join(chart, "Chart.yaml"));
    mkdirSync(path.join(chart, "templates"));
    cpSync(path.join(repo, "bridge/deploy/helm/kars-bridge/templates"), path.join(chart, "templates"), { recursive: true });
    const rendered = execFileSync("helm", ["template", "kars-local-idp", chart, "--show-only", "templates/idp.yaml", "--api-versions", "cilium.io/v2/CiliumNetworkPolicy", "-f", path.join(directory, "dex-values.yaml"), "--set", "idp.dex.kindGatewayCIDR=172.18.0.1/32"], { encoding: "utf8", stdio: "pipe" });
    const documents = parseAllDocuments(rendered).map(document => document.toJSON());
    const service = documents.find(doc => doc.kind === "Service");
    expect(service.spec).toMatchObject({ type: "NodePort", externalTrafficPolicy: "Local", ports: [{ port: 443, targetPort: "https", nodePort: 30443 }] });
    const pod = documents.find(doc => doc.kind === "Deployment").spec.template.spec;
    expect(pod.securityContext).toMatchObject({ runAsNonRoot: true, runAsUser: 1001, fsGroup: 1001 });
    expect(pod.volumes.find((volume: any) => volume.name === "storage").persistentVolumeClaim.claimName).toBe("dex-data");
    const config = parse(documents.find(doc => doc.kind === "ConfigMap").data["config.yaml"]);
    expect(config.web.http).toBeUndefined();
    expect(config.staticClients.map((client: any) => client.id)).toEqual(["kars-bridge", "kars-headlamp"]);
    expect(documents.find(doc => doc.kind === "NetworkPolicy").spec.egress).toEqual([]);
    const calls: string[] = [];
    const bootstrap = vi.fn<BetaExecute>(async (tool, args, options) => {
      calls.push(`${tool} ${args.join(" ")}`);
      if (tool === "helm") return { stdout: rendered };
      if (args[0] === "inspect") return { stdout: JSON.stringify([{ Id: nodeId, NetworkSettings: { Networks: { kind: { Gateway: "172.18.0.1" } } } }]) };
      if (args.includes("service")) return { stdout: JSON.stringify({ spec: { clusterIP: "10.96.0.9" } }) };
      if (args.includes("configmap")) return { stdout: JSON.stringify({ data: { Corefile: ".:53 { forward . /etc/resolv.conf }" } }) };
      if (args.includes("cat")) return { stdout: stringify({ kind: "Pod", metadata: { name: "kube-apiserver" }, spec: { hostNetwork: true } }) };
      if (args.includes("pods")) return { stdout: JSON.stringify({ items: [{ spec: { hostAliases: [{ ip: "10.96.0.9", hostnames: [state.trust!.hostname] }] }, status: { conditions: [{ type: "Ready", status: "True" }] } }] }) };
      if (tool === "kubectl" && options?.input) expect(args).toContain("--kubeconfig");
      return { stdout: "" };
    });
    const trust = vi.fn(async () => {}), verify = vi.fn(async () => {});
    await installLocalDex(plan, directory, state, tools, bootstrap, { trust, verify });
    expect(bootstrap.mock.calls.find(call => call[1].includes("install"))?.[1]).toEqual(["exec", nodeId, "install", "-d", "-m", "0700", "-o", "1001", "-g", "1001", `/var/local/kars-dex-${owner}`]);
    expect(calls.findIndex(call => call.includes("mv /etc/kubernetes/kars-oidc"))).toBeLessThan(calls.findIndex(call => call.includes("--raw /readyz")));
    expect(trust).toHaveBeenCalledWith(state, true, bootstrap);
    expect(verify).toHaveBeenCalledWith(plan);
    expect(readBetaState(directory)?.trust?.installed).toBe(true);
    const hostCommands = vi.fn<BetaExecute>(async () => ({ stdout: plan.ca! }));
    await changeManagedHostTrust(state, true, hostCommands, "darwin");
    expect(hostCommands.mock.calls[1][1]).toEqual(expect.arrayContaining(["add-trusted-cert", "-p", "ssl", "-s", state.trust!.hostname]));
    const hostScript = hostCommands.mock.calls[0][1][2];
    let hosts = "127.0.0.1 localhost\n", staged = "";
    const filesystem = { mkdirSync() {}, rmdirSync() {}, chownSync() {},
      lstatSync: () => ({ isFile: () => true, isSymbolicLink: () => false, mode: 0o644, uid: 0, gid: 0, ino: 1, mtimeMs: 1 }),
      readFileSync: () => hosts, writeFileSync: (_file: string, content: string) => { staged = content; }, renameSync: () => { hosts = staged; },
    };
    const runHostScript = new Function("require", "process", hostScript);
    runHostScript(() => filesystem, { argv: ["node", state.trust!.hostname, owner, "add"] });
    expect(hosts).toContain(`127.0.0.1 ${state.trust!.hostname} # kars-kind-owner=${owner}`);
    runHostScript(() => filesystem, { argv: ["node", state.trust!.hostname, owner, "remove"] });
    expect(hosts).toBe("127.0.0.1 localhost\n");
    await changeManagedHostTrust(state, false, hostCommands, "darwin");
    expect(hostCommands.mock.calls.some(call => call[1][0] === "delete-certificate" && call[1][2] === new X509Certificate(plan.ca!).fingerprint.replaceAll(":", ""))).toBe(true);
    const emptyRuntime = vi.fn<BetaExecute>(async () => ({ stdout: "" }));
    const cleanup = vi.fn(async () => { throw new Error("trust cleanup denied"); });
    await expect(teardownManagedBeta(directory, tools, emptyRuntime, cleanup)).rejects.toThrow(/cleanup denied/);
    expect(readBetaState(directory)).toMatchObject({ phase: "deleted", trust: { installed: true } });
    await teardownManagedBeta(directory, tools, emptyRuntime, async () => {});
    expect(emptyRuntime).toHaveBeenCalledOnce();
    expect(readBetaState(directory)?.trust?.installed).toBe(false);
  }, 30000);
  it("does not silently reuse saved inference when first setup requests configuration", async () => {
    const config = { provider: "foundry" as const, endpoint: "https://example.openai.azure.com", apiKey: "test-key", model: "model" };
    const prompt = vi.fn(async () => config);
    await prepareKindBetaInference({}, [], { load: () => config, prompt, interactive: true }, true);
    expect(prompt).toHaveBeenCalledOnce();
  });
  it("rejects bcrypt truncation and weak local passwords", () => {
    for (const password of ["short", "x".repeat(73), "é".repeat(37)]) expect(() => validateLocalBetaAccount({ email: "local@example.test", password })).toThrow();
  });
});
