// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

const enabled = process.env.BRIDGE_TEST_KIND_LIFECYCLE === "1";
const chart = fileURLToPath(new URL("../../deploy/helm/kars-bridge", import.meta.url));
const legacyChart = fileURLToPath(new URL("./fixtures/legacy-namespace-chart", import.meta.url));
const proxyCommand = fileURLToPath(new URL("../../deploy/configure-orchestrator-proxy.py", import.meta.url));
const kubeconfig = process.env.BRIDGE_TEST_KUBECONFIG;

function kubectl(args: string[], input?: unknown): string {
  if (!kubeconfig) throw new Error("A disposable Kind kubeconfig is required");
  return execFileSync("kubectl", ["--kubeconfig", kubeconfig, ...args], {
    encoding: "utf8",
    input: input === undefined ? undefined : JSON.stringify(input),
    stdio: ["pipe", "pipe", "pipe"],
    timeout: 30_000,
  });
}

function helm(args: string[]): string {
  if (!kubeconfig) throw new Error("A disposable Kind kubeconfig is required");
  return execFileSync("helm", ["--kubeconfig", kubeconfig, ...args], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 60_000,
  });
}

function uid(kind: string, name: string, namespace?: string): string {
  return kubectl([
    ...(namespace ? ["--namespace", namespace] : []),
    "get", kind, name, "-o", "jsonpath={.metadata.uid}",
  ]);
}

function seed(namespace: string): void {
  kubectl(["create", "-f", "-"], {
    apiVersion: "v1",
    kind: "List",
    items: [
      {
        apiVersion: "v1", kind: "ConfigMap",
        metadata: { name: "existing-kars-evidence", namespace },
        data: { evidence: "preserve-existing-core-data" },
      },
      {
        apiVersion: "apps/v1", kind: "Deployment",
        metadata: { name: "kars-controller", namespace },
        spec: {
          replicas: 0,
          selector: { matchLabels: { app: "existing-kars-controller" } },
          template: {
            metadata: { labels: { app: "existing-kars-controller" } },
            spec: { containers: [{ name: "controller", image: "ghcr.io/azure/kars-controller:latest" }] },
          },
        },
      },
      {
        apiVersion: "kars.azure.com/v1alpha1", kind: "BridgeLifecycleEvidence",
        metadata: { name: "existing-user-resource", namespace },
        spec: { evidence: "preserve-existing-custom-resource" },
      },
    ],
  });
}

function install(release: string, namespace: string, createNamespace: boolean): void {
  const releaseNamespace = createNamespace ? "bridge-lifecycle-metadata" : namespace;
  helm([
    "upgrade", "--install", release, chart, "--namespace", releaseNamespace,
    "--set", `namespace=${namespace},createNamespace=${createNamespace}`,
    "--set", "bff.replicas=0,web.replicas=0,teamsGateway.enabled=false,idp.enabled=false",
  ]);
}

describe.skipIf(!enabled)("real Helm add-on lifecycle in disposable Kind", () => {
  beforeAll(() => {
    // Never fall back to the caller's current context, especially a customer AKS cluster.
    const config = JSON.parse(kubectl(["config", "view", "--minify", "-o", "json"]));
    expect(config["current-context"]).toBe("kind-bridge-addon-lifecycle");
    expect(config.clusters[0].cluster.server).toMatch(/^https:\/\/127\.0\.0\.1:\d+$/);
    kubectl(["create", "namespace", "bridge-lifecycle-metadata"]);
    kubectl(["create", "-f", "-"], {
      apiVersion: "apiextensions.k8s.io/v1", kind: "CustomResourceDefinition",
      metadata: { name: "bridgelifecycleevidences.kars.azure.com" },
      spec: {
        group: "kars.azure.com",
        scope: "Namespaced",
        names: { plural: "bridgelifecycleevidences", singular: "bridgelifecycleevidence", kind: "BridgeLifecycleEvidence" },
        versions: [{
          name: "v1alpha1", served: true, storage: true,
          schema: { openAPIV3Schema: {
            type: "object",
            properties: { spec: { type: "object", properties: { evidence: { type: "string" } } } },
          } },
        }],
      },
    });
    kubectl(["wait", "--for=condition=Established", "--timeout=30s", "crd/bridgelifecycleevidences.kars.azure.com"]);
  }, 60_000);

  it.each([
    { namespace: "kars-system", managed: false, release: "bridge-shared" },
    { namespace: "bridge-lifecycle-dedicated", managed: true, release: "bridge-dedicated" },
  ])("preserves core and user resources in $namespace after removal", ({ namespace, managed, release }) => {
    const crdUid = uid("crd", "bridgelifecycleevidences.kars.azure.com");
    if (!managed) kubectl(["create", "namespace", namespace]);
    if (managed) install(release, namespace, true);
    seed(namespace);
    const namespaceUid = uid("namespace", namespace);
    const controllerUid = uid("deployment", "kars-controller", namespace);
    const evidenceUid = uid("configmap", "existing-kars-evidence", namespace);
    const customUid = uid("bridgelifecycleevidences", "existing-user-resource", namespace);

    install(release, namespace, managed);
    expect(uid("deployment", "kars-controller", namespace)).toBe(controllerUid);
    expect(uid("configmap", "existing-kars-evidence", namespace)).toBe(evidenceUid);
    expect(uid("bridgelifecycleevidences", "existing-user-resource", namespace)).toBe(customUid);
    helm(["uninstall", release, "--namespace", managed ? "bridge-lifecycle-metadata" : namespace, "--wait", "--timeout", "45s"]);

    expect(uid("namespace", namespace)).toBe(namespaceUid);
    expect(uid("crd", "bridgelifecycleevidences.kars.azure.com")).toBe(crdUid);
    expect(uid("deployment", "kars-controller", namespace)).toBe(controllerUid);
    expect(uid("configmap", "existing-kars-evidence", namespace)).toBe(evidenceUid);
    expect(uid("bridgelifecycleevidences", "existing-user-resource", namespace)).toBe(customUid);
    expect(kubectl(["--namespace", namespace, "get", "configmap", "existing-kars-evidence", "-o", "jsonpath={.data.evidence}"]))
      .toBe("preserve-existing-core-data");
    const deployments = JSON.parse(kubectl([
      "--namespace", namespace, "get", "deployment", "-l", "app.kubernetes.io/name=kars-bridge", "-o", "json",
    ]));
    expect(deployments.items).toHaveLength(0);
  }, 120_000);

  it("retains a legacy owned namespace even when an upgrade switches createNamespace off", () => {
    const namespace = "bridge-lifecycle-legacy";
    const release = "bridge-legacy";
    kubectl(["create", "-f", "-"], {
      apiVersion: "v1",
      kind: "Namespace",
      metadata: {
        name: namespace,
        labels: {
          "app.kubernetes.io/managed-by": "Helm",
          "app.kubernetes.io/name": "kars",
          "customer.example/namespace-policy": "preserve",
        },
        annotations: {
          "meta.helm.sh/release-name": release,
          "meta.helm.sh/release-namespace": namespace,
          "customer.example/namespace-setting": "preserve",
        },
      },
    });
    helm(["install", release, legacyChart, "--namespace", namespace]);
    expect(helm(["get", "manifest", release, "--namespace", namespace]))
      .not.toContain("helm.sh/resource-policy");
    seed(namespace);
    const namespaceUid = uid("namespace", namespace);
    const evidenceUid = uid("configmap", "existing-kars-evidence", namespace);
    const controllerUid = uid("deployment", "kars-controller", namespace);
    const customUid = uid("bridgelifecycleevidences", "existing-user-resource", namespace);

    install(release, namespace, false);
    expect(helm(["get", "manifest", release, "--namespace", namespace]))
      .toContain("helm.sh/resource-policy: keep");
    helm(["uninstall", release, "--namespace", namespace, "--wait", "--timeout", "45s"]);
    expect(uid("namespace", namespace)).toBe(namespaceUid);
    expect(uid("configmap", "existing-kars-evidence", namespace)).toBe(evidenceUid);
    expect(uid("deployment", "kars-controller", namespace)).toBe(controllerUid);
    expect(uid("bridgelifecycleevidences", "existing-user-resource", namespace)).toBe(customUid);
    const retained = JSON.parse(kubectl(["get", "namespace", namespace, "-o", "json"]));
    expect(retained.metadata.labels["app.kubernetes.io/name"]).toBe("kars");
    expect(retained.metadata.labels["customer.example/namespace-policy"]).toBe("preserve");
    expect(retained.metadata.annotations["customer.example/namespace-setting"]).toBe("preserve");
  }, 120_000);

  it("owns only the proxy allowance through enable, upgrade, disable and uninstall", () => {
    const namespace = "kars-system";
    const runtime = "kars-bridge-orchestrator";
    const release = "bridge-proxy-lifecycle";
    kubectl(["create", "-f", "-"], {
      apiVersion: "apiextensions.k8s.io/v1", kind: "CustomResourceDefinition",
      metadata: { name: "karssandboxes.kars.azure.com" },
      spec: {
        group: "kars.azure.com", scope: "Namespaced",
        names: { plural: "karssandboxes", singular: "karssandbox", kind: "KarsSandbox" },
        versions: [{
          name: "v1alpha1", served: true, storage: true,
          schema: { openAPIV3Schema: { type: "object" } },
        }],
      },
    });
    kubectl(["wait", "--for=condition=Established", "--timeout=30s", "crd/karssandboxes.kars.azure.com"]);
    kubectl(["create", "-f", "-"], {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox",
      metadata: { name: "bridge-orchestrator", namespace, labels: {
        "kars.azure.com/managed-by": "kars-bridge", "kars.azure.com/orchestrator": "true",
      } },
    });
    const sandboxUid = uid("karssandbox", "bridge-orchestrator", namespace);
    kubectl(["create", "-f", "-"], {
      apiVersion: "v1", kind: "Namespace", metadata: { name: runtime, annotations: {
        "kars.azure.com/sandbox-name": "bridge-orchestrator",
        "kars.azure.com/sandbox-namespace": namespace,
        "kars.azure.com/sandbox-uid": sandboxUid,
      } },
    });
    const namespaceUid = uid("namespace", runtime);
    // API/Helm lifecycle only: zero replicas deliberately do not claim AKS dataplane proof.
    kubectl(["create", "-f", "-"], {
      apiVersion: "apps/v1", kind: "Deployment",
      metadata: { name: "konnectivity-agent", namespace: "kube-system" },
      spec: { replicas: 0, selector: { matchLabels: { app: "konnectivity-agent" } },
        template: { metadata: { labels: { app: "konnectivity-agent" } }, spec: {
          hostNetwork: false, containers: [{ name: "agent", image: "example.invalid/proxy:latest" }],
        } } },
    });
    kubectl(["create", "-f", "-"], {
      apiVersion: "networking.k8s.io/v1", kind: "NetworkPolicy",
      metadata: { name: "sandbox-policy", namespace: runtime },
      spec: { podSelector: {}, policyTypes: ["Ingress", "Egress"], ingress: [], egress: [] },
    });
    const baseline = kubectl(["get", "networkpolicy", "sandbox-policy", "-n", runtime, "-o", "json"]);
    const controllerUid = uid("deployment", "kars-controller", namespace);
    install(release, namespace, false);
    const externalAnnotation = () => kubectl(["patch", "deployment", "kars-bridge-bff", "-n", namespace, "--type=merge",
      "-p", JSON.stringify({ spec: { template: { metadata: {
        annotations: { "test.kars/externally-managed-template": "preserve" },
      } } } })]);
    externalAnnotation();
    const bff = () => JSON.parse(kubectl(["get", "deployment", "kars-bridge-bff", "-n", namespace, "-o", "json"]));
    const revision = () => JSON.parse(helm(["status", release, "--namespace", namespace, "--output", "json"])).version;
    const proxyCheck = () => {
      if (!kubeconfig) throw new Error("Explicit disposable Kind kubeconfig required");
      return execFileSync("python3", [proxyCommand, "--kubeconfig", kubeconfig,
        "--context", "kind-bridge-addon-lifecycle", "--namespace", namespace, "--release", release, "--check"],
      { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 90_000 });
    };
    expect(JSON.parse(proxyCheck()).preflightPassed).toBe(true);
    const reviewedRevision = revision();
    const originalImage = bff().spec.template.spec.containers[0].image;
    kubectl(["set", "image", "deployment/kars-bridge-bff", "bff=example.invalid/drifted:latest", "-n", namespace]);
    expect(() => proxyCheck()).toThrow(/differs from the declared release/);
    expect(revision()).toBe(reviewedRevision);
    expect(bff().spec.template.spec.containers[0].image).toBe("example.invalid/drifted:latest");
    kubectl(["set", "image", "deployment/kars-bridge-bff", `bff=${originalImage}`, "-n", namespace]);
    kubectl(["delete", "deployment", "kars-bridge-bff", "-n", namespace, "--wait=true"]);
    expect(() => proxyCheck()).toThrow(/resources are missing or replaced/);
    expect(revision()).toBe(reviewedRevision);
    expect(kubectl(["get", "deployment", "kars-bridge-bff", "-n", namespace, "--ignore-not-found", "-o", "name"])).toBe("");
    install(release, namespace, false);
    externalAnnotation();
    const beforeBff = bff();
    const enable = ["upgrade", release, chart, "--namespace", namespace, "--reuse-values",
      "--set", "networkPolicy.orchestratorProxy.enabled=true",
      "--set-string", `networkPolicy.orchestratorProxy.namespaceUid=${namespaceUid}`,
      "--set-string", `networkPolicy.orchestratorProxy.sandboxUid=${sandboxUid}`];
    helm(enable);
    const policyName = `${release}-orchestrator-proxy`;
    const policyUid = uid("networkpolicy", policyName, runtime);
    helm(enable);
    expect(uid("networkpolicy", policyName, runtime)).toBe(policyUid);
    expect(bff().metadata.uid).toBe(beforeBff.metadata.uid);
    expect(bff().spec).toEqual(beforeBff.spec);
    helm(["upgrade", release, chart, "--namespace", namespace, "--reuse-values",
      "--set", "networkPolicy.orchestratorProxy.enabled=false"]);
    expect(kubectl(["get", "networkpolicy", policyName, "-n", runtime, "--ignore-not-found", "-o", "name"])).toBe("");
    helm(enable);
    expect(uid("networkpolicy", policyName, runtime)).not.toBe(policyUid);
    helm(["uninstall", release, "--namespace", namespace, "--wait", "--timeout", "45s"]);
    expect(kubectl(["get", "networkpolicy", policyName, "-n", runtime, "--ignore-not-found", "-o", "name"])).toBe("");
    expect(uid("namespace", runtime)).toBe(namespaceUid);
    expect(uid("karssandbox", "bridge-orchestrator", namespace)).toBe(sandboxUid);
    expect(uid("deployment", "kars-controller", namespace)).toBe(controllerUid);
    expect(kubectl(["get", "networkpolicy", "sandbox-policy", "-n", runtime, "-o", "json"])).toBe(baseline);
  }, 240_000);
});
