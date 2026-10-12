// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { execute, prompt } = vi.hoisted(() => ({ execute: vi.fn(), prompt: vi.fn() }));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("inquirer", () => ({ default: { prompt } }));

import { meshCommand } from "./mesh.js";

type Env = { name: string; value?: string; valueFrom?: unknown };
type Container = { name: string; image: string; env?: Env[]; envFrom?: unknown[] };
const audience: Env = { name: "AGENTMESH_ENTRA_AUDIENCE", value: "api://agentmesh" };
const tenant: Env = { name: "AGENTMESH_ENTRA_TENANT_ID", value: "tenant-id" };
const bypassName = "AGENTMESH_RELAY_ALLOW_UNAUTHED_DID";

function deployment(component: string, env?: Env[], name = component) {
  return {
    kind: "Deployment",
    metadata: {
      name,
      labels: { "kars.azure.com/mesh-provider": "agt" } as Record<string, string>,
      annotations: {} as Record<string, string>,
    },
    spec: { template: {
      metadata: { labels: { app: `agentmesh-${component}` } },
      spec: { containers: [{ name: component, image: `ghcr.io/microsoft/agent-governance-toolkit/${component}:latest`, env }] as Container[] },
    } },
    status: { readyReplicas: 1, conditions: [{ type: "Available", status: "True" }] },
  };
}

function mockRelay(env?: Env[], containers?: Container[]) {
  const resource = deployment("relay", env);
  if (containers) resource.spec.template.spec.containers = containers;
  execute.mockImplementation(async (bin: string, args: string[]) => {
    if (bin === "kubectl" && args[0] === "get" && args[1] === "deployment") {
      return { stdout: JSON.stringify(resource) };
    }
    throw new Error(`Unexpected command: ${bin} ${args.join(" ")}`);
  });
  return resource;
}

function mockInstallation(owner: "helm" | "standalone" | "external", prefixed = false) {
  const deployments = ["registry", "relay"].map(component =>
    deployment(component, [audience, tenant], prefixed ? `agentmesh-${component}` : component));
  const services = ["registry", "relay"].map(component => ({
    kind: "Service",
    metadata: {
      name: `agentmesh-${component}`,
      labels: {} as Record<string, string>,
      annotations: {} as Record<string, string>,
    },
    spec: { selector: { app: `agentmesh-${component}` } },
  }));
  const items = [...deployments, ...services];
  for (const item of items) {
    if (owner === "helm") {
      item.metadata.labels["app.kubernetes.io/managed-by"] = "Helm";
      item.metadata.annotations = { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" };
    } else if (owner === "external") {
      item.metadata.labels["app.kubernetes.io/managed-by"] = "another-controller";
    }
  }
  execute.mockImplementation(async (bin: string, args: string[]) => {
    if (bin === "kubectl" && args[0] === "get" && args[1] === "deployment,service") {
      return { stdout: JSON.stringify({ items }) };
    }
    if (bin === "kubectl" && args[0] === "get" && args[1] === "deployment") {
      return { stdout: JSON.stringify(deployments.find(item => item.metadata.name === args[2])) };
    }
    if (bin === "helm" && args[0] === "list") return { stdout: JSON.stringify([{ name: "kars", chart: "kars-0.1.0" }]) };
    if (bin === "helm" && args[0] === "get") return { stdout: JSON.stringify({ agentMesh: { enabled: true } }) };
    throw new Error(`Unexpected command: ${bin} ${args.join(" ")}`);
  });
  return { items, deployments, services };
}

function output(): string {
  return vi.mocked(console.log).mock.calls.map(args => args.join(" ")).join("\n");
}

function errors(): string {
  return vi.mocked(console.error).mock.calls.map(args => args.join(" ")).join("\n");
}

async function run(...args: string[]): Promise<void> {
  await meshCommand().parseAsync(["security", ...args], { from: "user" });
}

async function relayStatus(): Promise<void> {
  await run("status", "--deployment", "relay");
}

describe("mesh security configuration-only command", () => {
  let previousExitCode: typeof process.exitCode;

  beforeEach(() => {
    previousExitCode = process.exitCode;
    process.exitCode = undefined;
    execute.mockReset();
    prompt.mockReset();
    vi.spyOn(console, "log").mockImplementation(() => {});
    vi.spyOn(console, "error").mockImplementation(() => {});
  });

  afterEach(() => {
    try {
      expect(prompt).not.toHaveBeenCalled();
      for (const [bin, args] of execute.mock.calls as Array<[string, string[]]>) {
        expect(bin === "kubectl" && args[0] === "get" || bin === "helm" && ["get", "list"].includes(args[0])).toBe(true);
        expect(args.some(arg => arg.startsWith(`${bypassName}=`))).toBe(false);
      }
    } finally {
      process.exitCode = previousExitCode;
      vi.restoreAllMocks();
    }
  });

  it.each(["open", "strict", "OPEN", "Strict"])("refuses %s before any cluster call or confirmation", async mode => {
    await run(mode, "--namespace", "custom", "--deployment", "custom-relay");
    expect(process.exitCode).toBe(1);
    expect(execute).not.toHaveBeenCalled();
    expect(errors()).toContain("unsupported");
    expect(errors()).toContain("official Microsoft AGT ignores REQUIRE_REGISTRATION");
    expect(errors()).toContain("No changes made");
    expect(errors()).toContain("PoP) settings are preserved");
  });

  it("rejects an unknown mode without reading or mutating anything", async () => {
    await run("enable");
    expect(process.exitCode).toBe(1);
    expect(execute).not.toHaveBeenCalled();
    expect(errors()).toContain("Use: status");
  });

  it.each([undefined, "true", "false"])("never interprets REQUIRE_REGISTRATION=%s as enforcement", async value => {
    mockRelay(value === undefined ? undefined : [{ name: "REQUIRE_REGISTRATION", value }]);
    await relayStatus();
    expect(process.exitCode).toBeUndefined();
    expect(output()).toContain("ignored by official Microsoft AGT; not an enforcement signal");
    expect(output()).toContain("no bypass declared; official AGT defaults to requiring DID proof-of-possession");
    expect(output()).toContain("no pair declared in the deployment template");
    expect(output()).not.toContain("only registered agents may connect");
    expect(output()).not.toContain("Mode: STRICT");
    expect(output()).not.toContain("Mode: OPEN");
  });

  it.each(["helm", "standalone"] as const)("discovers %s relay and registry through the maintained ownership reader", async owner => {
    const resources = mockInstallation(owner);
    const before = JSON.stringify(resources);
    await run("STATUS");
    expect(process.exitCode).toBeUndefined();
    expect(output()).toContain("registry: agentmesh/registry");
    expect(output()).toContain("relay: agentmesh/relay");
    expect(output()).toContain(owner === "helm" ? "Helm kars-system/kars" : "Kars standalone manifest");
    expect(output().match(/paired literals declared; JWT acceptance NOT verified/g)).toHaveLength(2);
    expect(output()).toContain("healthy Pods do not prove JWT acceptance");
    expect(output()).toContain("Image (not verified)");
    expect(JSON.stringify(resources)).toBe(before);
    expect(execute.mock.calls.filter(([bin]) => bin === "helm")).toHaveLength(owner === "helm" ? 2 : 0);
  });

  it("discovers legacy prefixed deployment and container names without assuming relay", async () => {
    const { deployments } = mockInstallation("standalone", true);
    for (const resource of deployments) resource.spec.template.spec.containers[0].name = resource.metadata.name;
    await run("status");
    expect(process.exitCode).toBeUndefined();
    expect(output()).toContain("relay: agentmesh/agentmesh-relay");
    expect(execute).toHaveBeenCalledWith("kubectl", ["get", "deployment", "agentmesh-relay", "-n", "agentmesh", "-o", "json"], { stdio: "pipe" });
  });

  it("honors explicit custom namespace/deployment without assuming ownership or inspecting registry", async () => {
    mockRelay([audience, tenant]);
    await run("status", "-n", "external-mesh", "--deployment", "custom-relay");
    expect(process.exitCode).toBeUndefined();
    expect(execute).toHaveBeenCalledTimes(1);
    expect(execute).toHaveBeenCalledWith("kubectl", ["get", "deployment", "custom-relay", "-n", "external-mesh", "-o", "json"], { stdio: "pipe" });
    expect(output()).toContain("ownership not inferred; registry not inspected");
  });

  it("requires an explicit deployment outside the supported discovery namespace", async () => {
    await run("status", "--namespace", "custom");
    expect(process.exitCode).toBe(1);
    expect(execute).not.toHaveBeenCalled();
    expect(errors()).toContain("use --deployment");
  });

  it("does not guess a deployment for external ownership", async () => {
    mockInstallation("external");
    await run("status");
    expect(process.exitCode).toBe(1);
    expect(execute).toHaveBeenCalledTimes(1);
    expect(errors()).toContain("External or unrecognized AgentMesh ownership");
  });

  it("propagates mixed ownership instead of falling back to legacy names", async () => {
    const { services } = mockInstallation("helm");
    services[0].metadata.annotations = {};
    services[0].metadata.labels = {};
    await run("status");
    expect(process.exitCode).toBe(1);
    expect(execute).toHaveBeenCalledTimes(1);
    expect(errors()).toContain("mixed or incomplete Helm ownership");
  });

  it("reports a missing installation without claiming a default security state", async () => {
    execute.mockResolvedValue({ stdout: '{"items":[]}' });
    await run("status");
    expect(process.exitCode).toBe(1);
    expect(errors()).toContain("No AgentMesh installation found");
    expect(output()).not.toContain("PoP declaration");
  });

  it.each(["inventory denied", "connection refused"])("reports %s as a read failure, not secure defaults", async message => {
    execute.mockRejectedValue(new Error(message));
    await run("status");
    expect(process.exitCode).toBe(1);
    expect(execute).toHaveBeenCalledTimes(1);
    expect(errors()).toContain(message);
    expect(output()).not.toContain("no bypass declared");
  });

  it.each(["null", "{", "{}", '{"spec":{"template":{"spec":{"containers":[]}}}}'])("rejects unreadable deployment data %s", async stdout => {
    execute.mockResolvedValue({ stdout });
    await relayStatus();
    expect(process.exitCode).toBe(1);
    expect(errors()).toContain("Could not inspect AgentMesh configuration");
    expect(output()).not.toContain("PoP declaration");
  });

  it("fails on a deployment read error without a success verdict", async () => {
    execute.mockRejectedValue(new Error("deployment not found"));
    await relayStatus();
    expect(process.exitCode).toBe(1);
    expect(errors()).toContain("deployment not found");
    expect(output()).not.toContain("no pair declared");
  });

  it("selects the relay container rather than the first sidecar", async () => {
    mockRelay(undefined, [
      { name: "sidecar", image: "sidecar:latest", env: [audience, tenant, { name: bypassName, value: "true" }] },
      { name: "relay", image: "relay:latest" },
    ]);
    await relayStatus();
    expect(process.exitCode).toBeUndefined();
    expect(output()).toContain("no pair declared");
    expect(output()).not.toContain("WARNING: bypass declared");
  });

  it("refuses ambiguous containers instead of treating missing relay data as safe defaults", async () => {
    mockRelay(undefined, [{ name: "one", image: "one:latest" }, { name: "two", image: "two:latest" }]);
    await relayStatus();
    expect(process.exitCode).toBe(1);
    expect(errors()).toContain("Cannot identify relay container/image");
  });

  it.each(["1", "true", "TRUE", "yes"])("reports a declared PoP bypass %s without changing it", async value => {
    const resource = mockRelay([{ name: bypassName, value }]);
    const before = JSON.stringify(resource);
    await relayStatus();
    expect(process.exitCode).toBeUndefined();
    expect(output()).toContain("WARNING: bypass declared");
    expect(JSON.stringify(resource)).toBe(before);
  });

  it.each(["0", "false", "no", " true "])("matches official AGT non-bypass value %s without trimming", async value => {
    mockRelay([{ name: bypassName, value }]);
    await relayStatus();
    expect(output()).toContain("no bypass declared");
    expect(output()).not.toContain("WARNING: bypass declared");
  });

  it.each([[audience], [tenant], [audience, { ...tenant, value: "  " }]].map(env => ({ env })))("warns about an incomplete Entra pair $env", async ({ env }) => {
    mockRelay(env);
    await relayStatus();
    expect(process.exitCode).toBeUndefined();
    expect(output()).toContain("WARNING: incomplete pair; Entra verification may be disabled");
    expect(output()).not.toContain("paired literals declared");
  });

  it("does not treat a whitespace-only pair as configured", async () => {
    mockRelay([{ ...audience, value: " " }, { ...tenant, value: "\n" }]);
    await relayStatus();
    expect(output()).toContain("no pair declared in the deployment template");
    expect(output()).not.toContain("paired literals declared");
  });

  it.each([
    [{ ...audience, value: undefined, valueFrom: { secretKeyRef: { name: "auth", key: "audience" } } }, tenant],
    [{ ...audience, value: undefined, valueFrom: { configMapKeyRef: { name: "auth", key: "audience" } } }, tenant],
    [{ ...audience, value: "$(AUDIENCE)" }, tenant],
    [audience, { ...audience, value: "other-audience" }, tenant],
  ].map(env => ({ env })))("keeps referenced or ambiguous Entra configuration unknown: $env", async ({ env }) => {
    mockRelay(env);
    await relayStatus();
    expect(process.exitCode).toBeUndefined();
    expect(output()).toContain("unknown; both fields must be nonblank together");
    expect(output()).not.toContain("paired literals declared");
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it("does not infer defaults when envFrom could supply security settings", async () => {
    const resource = mockRelay();
    resource.spec.template.spec.containers[0].envFrom = [{ secretRef: { name: "mesh-auth" } }];
    await relayStatus();
    expect(output()).toContain("unknown (indirect or ambiguous environment)");
    expect(output()).toContain("unknown; both fields must be nonblank together");
    expect(output()).not.toContain("no bypass declared");
    expect(output()).not.toContain("no pair declared");
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it("honors explicit env overriding envFrom without resolving the references", async () => {
    const resource = mockRelay([audience, tenant, { name: bypassName, value: "false" }]);
    resource.spec.template.spec.containers[0].envFrom = [{ configMapRef: { name: "mesh-config" } }];
    await relayStatus();
    expect(output()).toContain("paired literals declared; JWT acceptance NOT verified");
    expect(output()).toContain("no bypass declared");
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it("keeps a referenced PoP bypass unknown without fetching credentials", async () => {
    mockRelay([{ name: bypassName, valueFrom: { secretKeyRef: { name: "auth", key: "pop" } } }]);
    await relayStatus();
    expect(output()).toContain("unknown (indirect or ambiguous environment)");
    expect(output()).not.toContain("no bypass declared");
    expect(execute).toHaveBeenCalledTimes(1);
  });

  it("documents unsupported modes in help while retaining peer registration", () => {
    const command = meshCommand();
    const security = command.commands.find(item => item.name() === "security")!;
    expect(security.description()).toContain("open/strict are unsupported");
    expect(security.options.find(option => option.long === "--deployment")?.defaultValue).toBeUndefined();
    const peer = command.commands.find(item => item.name() === "peer")!;
    expect(peer.description()).toContain("Toggle controller mesh federation");
    expect(peer.options.find(option => option.long === "--deployment")?.defaultValue).toBe("kars-controller");
  });
});
