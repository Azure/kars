// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { applyPushedImages } from "./push-apply.js";
import { controllerEnv, RUNTIME_IMAGE_TARGETS, type PushedImage } from "../lib/image-targets.js";
import type { Execute } from "../lib/deployment-target.js";
import type { DeploymentRecord } from "../lib/core-image-apply.js";
import { releaseImagePlan } from "../lib/release.js";
import { planSandboxImages } from "../lib/sandbox-image-apply.js";

const digest = `sha256:${"a".repeat(64)}`;
const pushed = (name: string): PushedImage => ({
  name, image: `mirror.azurecr.io/${name === "sandbox" ? "openclaw-sandbox" : name === "router" ? "kars-inference-router"
    : name === "relay" || name === "registry" ? `agentmesh-${name}-agt`
    : RUNTIME_IMAGE_TARGETS.find(item => item.name === name)?.repo ?? `kars-${name}`}:latest`,
});
const reference = (name: string) => `${pushed(name).image}@${digest}`;
const helmMetadata = () => ({
  labels: { "app.kubernetes.io/managed-by": "Helm" },
  annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" },
});
const healthy = () => ({ observedGeneration: 1, updatedReplicas: 1, readyReplicas: 1, conditions: [{ type: "Available", status: "True" }] });

function setPath(object: Record<string, unknown>, key: string, value: string) {
  const parts = key.split(".");
  let target = object;
  for (const part of parts.slice(0, -1)) target = (target[part] ??= {}) as Record<string, unknown>;
  target[parts.at(-1)!] = value;
}
function getPath(object: Record<string, unknown>, path: string): string {
  return path.split(".").reduce<unknown>((current, key) => (current as Record<string, unknown>)[key], object) as string;
}

function fixture(options: { legacyCore?: boolean; mesh?: "absent" | "helm" | "external"; fail?: string; mismatch?: boolean } = {}) {
  const values: Record<string, unknown> = {
    controller: { image: { repository: "ghcr.io/azure/kars-controller", tag: "v1.0.0", pullPolicy: "IfNotPresent" } },
    inferenceRouter: { image: { repository: "ghcr.io/azure/kars-inference-router", tag: "v1.0.0" } },
    sandbox: { image: { repository: "ghcr.io/azure/openclaw-sandbox", tag: "v1.0.0" } },
    azure: { workloadIdentity: { clientId: "customer-wi" }, keyVaultCsi: { keyVaultName: "customer-vault" } },
    agentMesh: { enabled: options.mesh === "helm" },
  };
  for (const runtime of RUNTIME_IMAGE_TARGETS) setPath(values, runtime.valueKey, `ghcr.io/azure/${runtime.repo}:v1.0.0`);
  const controller: DeploymentRecord = {
    metadata: { name: "kars-controller", namespace: "kars-system", resourceVersion: "1", generation: 1, ...(options.legacyCore ? {} : helmMetadata()) },
    spec: { replicas: 1, template: { spec: { containers: [{
      name: "controller", image: "ghcr.io/azure/kars-controller:v1.0.0", imagePullPolicy: "IfNotPresent",
      env: [
        { name: "SANDBOX_IMAGE", value: "ghcr.io/azure/openclaw-sandbox:v1.0.0" },
        { name: "INFERENCE_ROUTER_IMAGE", value: "ghcr.io/azure/kars-inference-router:v1.0.0" },
        ...RUNTIME_IMAGE_TARGETS.map(item => ({ name: item.env, value: getPath(values, item.valueKey) })),
        { name: "CUSTOMER_SETTING", value: "retain-me" },
      ],
    }] } } }, status: healthy(),
  };
  const crs: Array<{ metadata: { name: string; namespace: string }; spec: { runtime: Record<string, unknown> } }> = [];
  const sandboxes: DeploymentRecord[] = [];
  const meshResources = options.mesh && options.mesh !== "absent" ? ["registry", "relay"].flatMap(component => {
    const metadata = options.mesh === "helm" ? helmMetadata() : {
      labels: { "app.kubernetes.io/managed-by": "platform-controller" }, annotations: {},
    };
    return [
      { kind: "Deployment", metadata: { name: options.mesh === "external" ? `platform-${component}` : component, namespace: "agentmesh", generation: 1, ...metadata },
        spec: { replicas: 1, template: { metadata: { labels: { app: `platform-${component}` } },
          spec: { containers: [{ name: component, image: `old.example/${component}:old` }] } } }, status: healthy() },
      { kind: "Service", metadata: { name: `agentmesh-${component}`, ...metadata }, spec: { selector: { app: `platform-${component}` } } },
    ];
  }) : [];
  function addSandbox(name: string, kind: string, variant: string, config: Record<string, unknown> = {}) {
    const cr = { metadata: { name, namespace: "kars-system" }, spec: { runtime: { kind, [variant]: config } } };
    crs.push(cr);
    const runtime = RUNTIME_IMAGE_TARGETS.find(item => item.kind === kind && (!item.language || (config.language ?? "python") === item.language));
    const agentImage = typeof config.image === "string" ? config.image
      : kind === "OpenClaw" ? "ghcr.io/azure/openclaw-sandbox:v1.0.0" : `ghcr.io/azure/${runtime?.repo}:v1.0.0`;
    const deployment: DeploymentRecord = {
      metadata: { name, namespace: `kars-${name}`, generation: 1, labels: { "kars.azure.com/sandbox": name, "kars.azure.com/parent-namespace": "kars-system" } },
      spec: { replicas: 1, template: { spec: {
        containers: [{ name: kind === "OpenClaw" ? "openclaw" : "agent", image: agentImage },
          { name: "inference-router", image: "ghcr.io/azure/kars-inference-router:v1.0.0" }],
        initContainers: [{ name: "egress-guard", image: "ghcr.io/azure/openclaw-sandbox:v1.0.0" }],
      } } }, status: healthy(),
    };
    sandboxes.push(deployment);
    return { cr, deployment };
  }
  function materialize(name: string) {
    if (options.mismatch) return;
    const cr = crs.find(item => item.metadata.name === name)!;
    const deployment = sandboxes.find(item => item.metadata.name === name)!;
    const vars = controller.spec.template.spec.containers[0].env!;
    const image = (key: string) => vars.find(item => item.name === key)!.value!;
    deployment.spec.template.spec.containers.find(item => item.name === "inference-router")!.image = image("INFERENCE_ROUTER_IMAGE");
    deployment.spec.template.spec.initContainers![0].image = image("SANDBOX_IMAGE");
    const kind = cr.spec.runtime.kind;
    const target = RUNTIME_IMAGE_TARGETS.find(item => item.kind === kind
      && (!item.language || ((cr.spec.runtime[item.variant] as { language?: string })?.language ?? "python") === item.language));
    const config = cr.spec.runtime[kind === "OpenClaw" ? "openclaw" : target?.variant ?? "byo"] as { image?: string };
    deployment.spec.template.spec.containers[0].image = config?.image ?? image(kind === "OpenClaw" ? "SANDBOX_IMAGE" : target!.env);
  }
  const execute = vi.fn(async (bin: string, args: readonly string[]) => {
    if (options.fail && (args[0] === options.fail || args.includes(options.fail))) throw new Error(`failed ${options.fail}`);
    if (bin === "az") return { stdout: digest };
    if (bin === "helm" && args[0] === "list") return { stdout: JSON.stringify([{ name: "kars", chart: "kars-0.1.0" }]) };
    if (bin === "helm" && args[0] === "get") return { stdout: JSON.stringify(values) };
    if (bin === "helm" && args[0] === "upgrade") {
      for (const arg of args.filter(arg => arg.includes("="))) {
        const at = arg.indexOf("=");
        setPath(values, arg.slice(0, at), arg.slice(at + 1));
      }
      const c = controller.spec.template.spec.containers[0];
      if (!options.mismatch) c.image = `${getPath(values, "controller.image.repository")}:${getPath(values, "controller.image.tag")}`;
      for (const name of ["router", "sandbox", ...RUNTIME_IMAGE_TARGETS.map(item => item.name)]) {
        const target = RUNTIME_IMAGE_TARGETS.find(item => item.name === name);
        const key = name === "router" ? "inferenceRouter" : name;
        c.env!.find(item => item.name === controllerEnv(name))!.value = target ? getPath(values, target.valueKey)
          : `${getPath(values, `${key}.image.repository`)}:${getPath(values, `${key}.image.tag`)}`;
      }
      for (const resource of meshResources.filter(item => options.mesh === "helm" && item.kind === "Deployment")) {
        const component = resource.metadata.name;
        const prefix = `agentMesh.${component}.image`;
        resource.spec.template!.spec.containers[0].image = `${getPath(values, `${prefix}.repository`)}:${getPath(values, `${prefix}.tag`)}`;
      }
    }
    if (bin === "kubectl" && args[0] === "patch") {
      const ops = JSON.parse(args[args.indexOf("-p") + 1]) as Array<{ op: string; path: string; value: unknown }>;
      for (const op of ops.filter(item => item.op !== "test")) {
        const c = controller.spec.template.spec.containers[0];
        if (op.path.endsWith("/env")) c.env = op.value as typeof c.env;
        if (op.path.endsWith("/image")) c.image = op.value as string;
        if (op.path.endsWith("/imagePullPolicy")) c.imagePullPolicy = op.value as string;
      }
    }
    if (bin === "kubectl" && args[0] === "annotate") materialize(args[2]);
    if (bin === "kubectl" && args[0] === "get") {
      if (args[1] === "deployment,service") return { stdout: JSON.stringify({ items: meshResources }) };
      if (args[1] === "deployments") return { stdout: JSON.stringify({ items: sandboxes }) };
      if (args[1] === "karssandboxes") return { stdout: JSON.stringify({ items: crs }) };
      if (args[1] === "karssandbox") return { stdout: JSON.stringify(crs.find(cr => cr.metadata.name === args[2])) };
      if (args[1] === "deployment") return { stdout: JSON.stringify(args[2] === "kars-controller" ? controller
        : sandboxes.find(item => item.metadata.name === args[2]) ?? meshResources.find(item => item.kind === "Deployment" && item.metadata.name === args[2])) };
    }
    return { stdout: "" };
  }) as unknown as Execute;
  const calls = () => vi.mocked(execute).mock.calls as unknown as Array<[string, string[]]>;
  return { execute, values, controller, addSandbox, calls, meshResources };
}

describe("selected core push artifacts", () => {
  it("moves a GHCR/pinned Helm controller to its pushed ACR digest without resetting customer values", async () => {
    const f = fixture();
    await applyPushedImages(f.execute, [pushed("controller")], "chart");
    expect(f.controller.spec.template.spec.containers[0].image).toBe(reference("controller"));
    expect(getPath(f.values, "azure.workloadIdentity.clientId")).toBe("customer-wi");
    expect(getPath(f.values, "azure.keyVaultCsi.keyVaultName")).toBe("customer-vault");
    expect(getPath(f.values, "inferenceRouter.image.tag")).toBe("v1.0.0");
    expect(f.calls().filter(([bin, args]) => bin === "helm" && args[0] === "upgrade")).toHaveLength(1);
  });

  it("updates unmanaged controller image/env through version-guarded patches, without Helm", async () => {
    const f = fixture({ legacyCore: true });
    const agent = f.addSandbox("agent", "OpenClaw", "openclaw");
    await applyPushedImages(f.execute, [pushed("controller"), pushed("router")], "chart");
    expect(f.calls().some(([bin]) => bin === "helm")).toBe(false);
    expect(f.controller.spec.template.spec.containers[0].image).toBe(reference("controller"));
    expect(agent.deployment.spec.template.spec.containers[1].image).toBe(reference("router"));
    expect(f.controller.spec.template.spec.containers[0].env).toContainEqual({ name: "CUSTOMER_SETTING", value: "retain-me" });
    expect(f.calls().find(([, args]) => args[0] === "patch")?.[1].at(-1)).toContain("/metadata/resourceVersion");
  });

  it.each(["router", "sandbox"])("applies %s defaults and eligible workloads but preserves explicit agent pins", async name => {
    const f = fixture();
    const normal = f.addSandbox("normal", "OpenClaw", "openclaw");
    const custom = f.addSandbox("custom", "OpenClaw", "openclaw", { image: "customer.example/custom:v9" });
    const spec = JSON.stringify(custom.cr.spec);
    await applyPushedImages(f.execute, [pushed(name)], "chart");
    const index = name === "router" ? 1 : 0;
    expect(normal.deployment.spec.template.spec.containers[index].image).toBe(reference(name));
    expect(custom.deployment.spec.template.spec.containers[0].image).toBe("customer.example/custom:v9");
    expect(JSON.stringify(custom.cr.spec)).toBe(spec);
    if (name === "sandbox") expect(custom.deployment.spec.template.spec.initContainers![0].image).toBe(reference(name));
    expect(f.calls().some(([, args]) => args[0] === "patch" && args.includes("karssandbox"))).toBe(false);
  });

  it.each(RUNTIME_IMAGE_TARGETS)("applies complete runtime mapping $name and leaves other runtimes untouched", async runtime => {
    const f = fixture();
    const selected = f.addSandbox("selected", runtime.kind, runtime.variant, runtime.language ? { language: runtime.language } : {});
    const other = f.addSandbox("other", "OpenClaw", "openclaw");
    const custom = f.addSandbox("custom", runtime.kind, runtime.variant, { image: "customer.example/runtime:v7", ...(runtime.language ? { language: runtime.language } : {}) });
    await applyPushedImages(f.execute, [pushed(runtime.name)], "chart");
    expect(getPath(f.values, runtime.valueKey)).toBe(reference(runtime.name));
    expect(selected.deployment.spec.template.spec.containers[0].image).toBe(reference(runtime.name));
    expect(other.deployment.spec.template.spec.containers[0].image).toBe("ghcr.io/azure/openclaw-sandbox:v1.0.0");
    expect(custom.deployment.spec.template.spec.containers[0].image).toBe("customer.example/runtime:v7");
    expect(releaseImagePlan("v1.2.3").some(item => item.target === `${runtime.repo}:latest`)).toBe(true);
  });

  it("combines core and owned mesh in one Helm upgrade", async () => {
    const f = fixture({ mesh: "helm" });
    await applyPushedImages(f.execute, [pushed("controller"), pushed("relay"), pushed("registry")], "chart");
    const updates = f.calls().filter(([bin, args]) => bin === "helm" && args[0] === "upgrade");
    expect(updates).toHaveLength(1);
    expect(updates[0][1]).toContain(`agentMesh.relay.image.tag=latest@${digest}`);
    expect(updates[0][1]).toContain(`controller.image.tag=latest@${digest}`);
    expect(updates[0][1]).not.toContain("--take-ownership");
  });

  it("leaves external mesh untouched during a core-only apply", async () => {
    const f = fixture({ mesh: "external" });
    const before = JSON.stringify(f.meshResources);
    await applyPushedImages(f.execute, [pushed("controller")], "chart");
    expect(JSON.stringify(f.meshResources)).toBe(before);
    expect(f.calls().some(([, args]) => args[0] === "rollout" && args.includes("agentmesh"))).toBe(false);
    expect(f.calls().some(([, args]) => args[0] === "get" && args[1] === "deployment" && args.includes("agentmesh"))).toBe(false);
  });

  it("refuses explicit or default-all external mesh updates before any mutation", async () => {
    for (const images of [[pushed("relay")], [pushed("controller"), pushed("relay"), pushed("registry")]]) {
      const f = fixture({ mesh: "external" });
      await expect(applyPushedImages(f.execute, images, "chart")).rejects.toThrow("External");
      expect(f.calls().some(([, args]) => ["upgrade", "patch", "set", "annotate", "rollout"].includes(args[0]))).toBe(false);
    }
  });

  it("rejects a build-only apply and reports a mixed build-only target as not deployed", async () => {
    const f = fixture();
    await expect(applyPushedImages(f.execute, [pushed("sandbox-base")], "chart")).rejects.toThrow("build-only");
    expect(f.calls()).toHaveLength(0);
    const result = await applyPushedImages(f.execute, [pushed("controller"), pushed("sandbox-base")], "chart");
    expect(result.buildOnly).toEqual(["sandbox-base"]);
    expect(result.applied).toEqual(["controller"]);
  });

  it("propagates a failed owning-config update", async () => {
    const f = fixture({ fail: "upgrade" });
    await expect(applyPushedImages(f.execute, [pushed("controller")], "chart")).rejects.toThrow("failed upgrade");
    expect(f.calls().some(([, args]) => args[0] === "rollout")).toBe(false);
  });

  it("does not report applied when Helm accepts values but leaves the controller on old bits", async () => {
    const f = fixture({ mismatch: true });
    await expect(applyPushedImages(f.execute, [pushed("controller")], "chart")).rejects.toThrow("selected artifact");
  });

  it("fails before configuration mutations when the pushed digest cannot be resolved", async () => {
    const f = fixture({ fail: "repository" });
    await expect(applyPushedImages(f.execute, [pushed("controller")], "chart")).rejects.toThrow("failed repository");
    expect(f.calls().some(([, args]) => ["upgrade", "patch", "annotate", "rollout"].includes(args[0]))).toBe(false);
  });

  it("distinguishes LangGraph TypeScript from Python for runtime-only application", async () => {
    const f = fixture();
    const python = f.addSandbox("python", "LangGraph", "langGraph", { language: "python" });
    const typescript = f.addSandbox("typescript", "LangGraph", "langGraph", { language: "typescript" });
    const oldPython = python.deployment.spec.template.spec.containers[0].image;
    await applyPushedImages(f.execute, [pushed("runtime-langgraph-ts")], "chart");
    expect(python.deployment.spec.template.spec.containers[0].image).toBe(oldPython);
    expect(typescript.deployment.spec.template.spec.containers[0].image).toBe(reference("runtime-langgraph-ts"));
  });

  it("rejects actual workload mismatch even if restart/status commands succeed", async () => {
    const f = fixture({ mismatch: true });
    f.addSandbox("agent", "Hermes", "hermes");
    await expect(applyPushedImages(f.execute, [pushed("runtime-hermes")], "chart")).rejects.toThrow("did not converge");
  });

  it("does not reset BYO pins or touch overlay workloads", () => {
    const f = fixture();
    const byo = f.addSandbox("byo", "BYO", "byo", { image: "customer.example/byo:v1" });
    const plan = planSandboxImages(byo.cr, byo.deployment, [pushed("sandbox")]);
    expect(plan.expected.every(item => item.init)).toBe(true);
    const overlay = { ...byo.cr, spec: { ...byo.cr.spec, upstreamCompatibility: { sigsAgentSandbox: "overlay" } } };
    expect(planSandboxImages(overlay, byo.deployment, [pushed("router")]).expected).toEqual([]);
  });
});
