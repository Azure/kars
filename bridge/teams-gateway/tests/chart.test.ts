// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  loadYaml,
  type KubernetesObject,
  type V1ClusterRole,
  type V1Deployment,
  type V1Namespace,
  type V1NetworkPolicy,
  type V1Role,
  type V1RoleBinding,
} from "@kubernetes/client-node";
import { describe, expect, it } from "vitest";

const chart = fileURLToPath(new URL("../../deploy/helm/kars-bridge", import.meta.url));
const requiredApis = [
  "karssandboxes", "karstasks", "karsteams", "karsprofiles", "karsskills",
  "karsapprovals", "egressapprovals", "karsreceipts", "mcpservers",
  "inferencepolicies", "toolpolicies", "karsmemories", "karsevals", "karssreactions", "karscredentialgrants",
];

function render(...args: string[]): KubernetesObject[] {
  const yaml = execFileSync(
    "helm",
    ["template", "kars-bridge", chart, "--include-crds", "--namespace", "kars-system", ...args],
    { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 10_000 },
  );
  // Helm may emit comment-only documents for disabled optional templates.
  return yaml.split(/^---\s*$/m)
    .filter((doc) => doc.split("\n").some((line) => line.trim() && !line.trimStart().startsWith("#")))
    .map((doc) => loadYaml(doc));
}

function resource<T extends KubernetesObject>(
  resources: KubernetesObject[],
  kind: string,
  name: string,
): T {
  const found = resources.find((item) => item.kind === kind && item.metadata?.name === name);
  expect(found, `${kind}/${name}`).toBeDefined();
  return found as T;
}

const scenarios = [
  { name: "defaults", args: [] },
  { name: "shared Kars namespace", args: ["--set", "namespace=kars-system,createNamespace=false"] },
  { name: "dedicated namespace", args: ["--set", "namespace=bridge-workspace,createNamespace=true"] },
  { name: "existing custom namespace", args: ["--set", "namespace=shared-workspace,createNamespace=false"] },
  {
    name: "optional surfaces enabled",
    args: ["--set", "idp.enabled=true,teamsGateway.enabled=true,ingress.enabled=true,ingress.host=bridge.example.test"],
  },
];

describe("Bridge optional add-on boundary (offline Helm manifests)", () => {
  it("opens observation egress only for explicitly reviewed existing isolation and targets", () => {
    expect(render().some((item) => item.metadata?.name === "kars-bridge-observation-egress")).toBe(false);
    expect(() => render("--set", "networkPolicy.observations.enabled=true")).toThrow(/already egress-isolated/);
    expect(() => render("--set", "networkPolicy.observations.enabled=true,networkPolicy.observations.existingIsolationConfirmed=true"))
      .toThrow(/exact reviewed targetNamespaces/);
    const policy = resource<V1NetworkPolicy>(render("--set",
      "namespace=bridge-private,core.namespace=core-workspace,networkPolicy.observations.enabled=true,networkPolicy.observations.existingIsolationConfirmed=true,networkPolicy.observations.targetNamespaces[0]=kars-agent"),
      "NetworkPolicy", "kars-bridge-observation-egress");
    expect(policy.metadata?.namespace).toBe("bridge-private");
    expect(policy.spec?.policyTypes).toEqual(["Egress"]);
    expect(policy.spec?.egress?.[0]?.ports).toEqual([{port:9447,protocol:"TCP"}]);
    expect(policy.spec?.egress?.[0]?.to?.[0]?.namespaceSelector?.matchExpressions?.[0]?.values).toEqual(["kars-agent"]);
    expect(policy.spec?.podSelector.matchLabels?.["app.kubernetes.io/component"]).toBe("bff");
  });
  for (const scenario of scenarios) {
    it(`owns only add-on resources with ${scenario.name}`, () => {
      const resources = render(...scenario.args);
      expect(resources.length).toBeGreaterThan(0);
      const allowedKinds = new Set([
        "Namespace", "Deployment", "Service", "ServiceAccount", "ConfigMap", "Secret",
        "ClusterRole", "ClusterRoleBinding", "Role", "RoleBinding", "NetworkPolicy", "Ingress",
      ]);
      for (const item of resources) {
        expect(allowedKinds.has(item.kind!), item.kind).toBe(true);
        expect(item.apiVersion).not.toMatch(/^kars\.azure\.com\//);
        expect(item.metadata?.labels?.["app.kubernetes.io/name"]).toBe("kars-bridge");
        expect(item.metadata?.annotations?.["helm.sh/hook"]).toBeUndefined();
        expect(item.metadata?.ownerReferences ?? []).toEqual([]);
        if (item.kind === "Namespace") {
          expect(scenario.name).toBe("dedicated namespace");
          expect(item.metadata?.name).toBe("bridge-workspace");
          expect(item.metadata?.annotations?.["helm.sh/resource-policy"]).toBe("keep");
        } else {
          expect(item.metadata?.name).toMatch(/^(kars-bridge(?:-|$)|kars-teams-conversations$|dex$)/);
        }
      }
      expect(resources.some((item) => item.metadata?.name === "kars-controller")).toBe(false);
      expect(resources.some((item) => item.metadata?.name === "kars-system")).toBe(false);

      for (const item of resources.filter((item) => item.kind === "NetworkPolicy")) {
        const selector = (item as V1NetworkPolicy).spec?.podSelector;
        expect(selector?.matchLabels?.["app.kubernetes.io/name"] ?? selector?.matchLabels?.app)
          .toMatch(/^(kars-bridge|dex)$/);
      }

      for (const item of resources.filter((item) => ["ClusterRole", "Role"].includes(item.kind!))) {
        for (const rule of (item as V1Role).rules ?? []) {
          expect(rule.apiGroups).not.toContain("*");
          expect(rule.resources).not.toContain("*");
          expect(rule.verbs).not.toContain("*");
          if (rule.resources?.includes("customresourcedefinitions")) {
            expect(rule.verbs?.every((verb) => ["get", "list", "watch"].includes(verb))).toBe(true);
          }
          if (rule.resources?.some((name) => ["namespaces", "deployments"].includes(name))) {
            expect(rule.verbs).not.toContain("delete");
            expect(rule.verbs).not.toContain("deletecollection");
          }
        }
      }
    });
  }

  it("never claims the shared Kars namespace on a new install", () => {
    for (const namespace of ["kars-system", ""]) {
      expect(() => render("--set", `namespace=${namespace},createNamespace=true`))
        .toThrow(/Bridge must not own the shared kars-system namespace/);
    }
  });

  it("keeps a legacy owned shared namespace in upgrades so retention can be applied safely", () => {
    const resources = render("--is-upgrade", "--set", "namespace=kars-system,createNamespace=true");
    const namespace = resource<V1Namespace>(resources, "Namespace", "kars-system");
    expect(namespace.metadata?.annotations?.["helm.sh/resource-policy"]).toBe("keep");
  });

  it("rejects trying to create the release storage namespace from its own chart", () => {
    expect(() => render(
      "--namespace", "bridge-workspace",
      "--set", "namespace=bridge-workspace,createNamespace=true",
    )).toThrow(/cannot bootstrap its own Helm release storage namespace/);
  });

  it("retains a dedicated namespace on install and upgrade to prevent cascading data deletion", () => {
    for (const upgradeArgs of [[], ["--is-upgrade"]]) {
      const resources = render(
        "--set", "namespace=bridge-workspace,createNamespace=true", ...upgradeArgs,
      );
      const namespaces = resources.filter((item) => item.kind === "Namespace");
      expect(namespaces).toHaveLength(1);
      const namespace = namespaces[0] as V1Namespace;
      expect(namespace.metadata?.name).toBe("bridge-workspace");
      expect(namespace.metadata?.annotations?.["helm.sh/resource-policy"]).toBe("keep");
      for (const item of resources.filter((item) => item.metadata?.namespace)) {
        const coreRead = ["Role", "RoleBinding"].includes(item.kind!)
          && item.metadata?.name === "kars-bridge-bridge-workspace-teams-gateway-core-read";
        expect(item.metadata?.namespace).toBe(coreRead ? "kars-system" : "bridge-workspace");
      }
    }
  });

  it("does not manage existing namespaces on install or upgrade", () => {
    for (const namespace of ["kars-system", "shared-workspace"]) {
      for (const upgradeArgs of [[], ["--is-upgrade"]]) {
        const resources = render("--set", `namespace=${namespace},createNamespace=false`, ...upgradeArgs);
        expect(resources.filter((item) => item.kind === "Namespace")).toEqual([]);
      }
    }
  });

  it("wires fail-closed readiness separately from process liveness", () => {
    const resources = render();
    const bff = resource<V1Deployment>(resources, "Deployment", "kars-bridge-bff");
    const container = bff.spec!.template.spec!.containers[0]!;
    expect(container.readinessProbe?.httpGet?.path).toBe("/readyz");
    expect(container.readinessProbe?.timeoutSeconds).toBeGreaterThan(5);
    expect(container.livenessProbe?.httpGet?.path).toBe("/healthz");
    const role = resource<V1ClusterRole>(resources, "ClusterRole", "kars-bridge-kars-bridge");
    for (const api of requiredApis) {
      expect(role.rules?.some((rule) =>
        rule.apiGroups?.some((group) => group === "kars.azure.com")
        && rule.resources?.includes(api)
        && rule.verbs?.includes("list"),
      ), `readiness list permission for ${api}`).toBe(true);
    }
  });

  it("keeps the web image immutable while allowing only bounded cache and temporary writes", () => {
    const resources = render();
    const web = resource<V1Deployment>(resources, "Deployment", "kars-bridge-web");
    const pod = web.spec!.template.spec!;
    const container = pod.containers[0]!;
    expect(container.securityContext?.readOnlyRootFilesystem).toBe(true);
    expect(pod.securityContext?.fsGroup).toBe(10001);
    expect(container.volumeMounts?.map((mount) => mount.mountPath).sort())
      .toEqual(["/app/.next/cache", "/tmp"]);
    expect(pod.volumes?.every((volume) => volume.emptyDir?.sizeLimit)).toBe(true);
    const custom = resource<V1Deployment>(
      render("--set", "podSecurityContext.fsGroup=20001"), "Deployment", "kars-bridge-web",
    );
    expect(custom.spec?.template.spec?.securityContext?.fsGroup).toBe(20001);
  });

  it("does not require tenant credentials or a running Teams gateway for web-only use", () => {
    const resources = render("--set", "teamsGateway.replicas=3");
    const gateway = resource<V1Deployment>(resources, "Deployment", "kars-bridge-teams-gateway");
    expect(gateway.spec?.replicas).toBe(0);
    expect(resources.some((item) => item.kind === "Secret")).toBe(false);
    for (const name of ["kars-bridge-bff", "kars-bridge-web"]) {
      const deployment = resource<V1Deployment>(resources, "Deployment", name);
      expect(deployment.spec?.replicas).toBe(1);
      for (const container of deployment.spec!.template.spec!.containers) {
        for (const env of container.env ?? []) {
          if (env.valueFrom?.secretKeyRef) {
            expect(env.valueFrom.secretKeyRef.optional, env.name).toBe(true);
            expect(env.valueFrom.secretKeyRef.name).toBe("kars-bridge-teams");
          }
        }
      }
    }
    const bff = resource<V1Deployment>(resources, "Deployment", "kars-bridge-bff");
    const env = bff.spec!.template.spec!.containers[0]!.env!;
    for (const name of ["BRIDGE_TEAMS_INTERNAL_SECRET", "BRIDGE_TEAMS_ENTRA_ROLE_MAP"]) {
      expect(env.find((item) => item.name === name)?.valueFrom?.secretKeyRef?.optional).toBe(true);
    }
  });

  it("starts gateway replicas only after explicit enablement", () => {
    const resources = render("--set", "teamsGateway.enabled=true,teamsGateway.replicas=2");
    expect(resource<V1Deployment>(resources, "Deployment", "kars-bridge-teams-gateway").spec?.replicas)
      .toBe(2);
  });

  it.each([
    { namespace: "bridge-private", core: "kars-system" },
    { namespace: "bridge-private", core: "core-workspace" },
    { namespace: "kars-system", core: "kars-system" },
    { namespace: "shared-workspace", core: "shared-workspace" },
  ].flatMap((scenario) => [false, true].map((upgrade) => ({ ...scenario, upgrade }))))(
    "keeps gateway commands/watches in $core and storage/identity in $namespace (upgrade=$upgrade)", ({ namespace, core, upgrade }) => {
    const resources = render("--set",
      `namespace=${namespace},core.namespace=${core},teamsGateway.enabled=true,createNamespace=false,teamsGateway.conversationConfigMapName=custom-store`,
      ...(upgrade ? ["--is-upgrade"] : []));
    const gateway = resource<V1Deployment>(resources, "Deployment", "kars-bridge-teams-gateway");
    const pod = gateway.spec!.template.spec!;
    const env = pod.containers[0]!.env!;
    expect(gateway.metadata?.namespace).toBe(namespace);
    expect(pod.serviceAccountName).toBe("kars-bridge-teams-gateway");
    expect(env.find((item) => item.name === "TEAMS_CONFIGMAP_NAMESPACE")?.value).toBe(namespace);
    expect(env.find((item) => item.name === "TEAMS_CONFIGMAP_NAME")?.value).toBe("custom-store");
    expect(env.find((item) => item.name === "TEAMS_WATCH_NAMESPACE")?.value).toBe(core);
    expect(env.find((item) => item.name === "TEAMS_BFF_BASE_URL")?.value)
      .toBe(`http://kars-bridge-bff.${namespace}.svc.cluster.local:8081`);
    expect(resource(resources, "ConfigMap", "custom-store").metadata?.namespace).toBe(namespace);
    expect(resource(resources, "ServiceAccount", pod.serviceAccountName!).metadata?.namespace).toBe(namespace);
    expect(resource(resources, "Service", "kars-bridge-teams-gateway").metadata?.namespace).toBe(namespace);
    const coreName = `kars-bridge-${namespace}-teams-gateway-core-read`;
    const storageRole = resource<V1Role>(resources, "Role", "kars-bridge-teams-gateway");
    const coreRole = resource<V1Role>(resources, "Role", coreName);
    expect(storageRole.metadata?.namespace).toBe(namespace);
    expect(storageRole.rules).toEqual([{
      apiGroups: [""], resources: ["configmaps"], resourceNames: ["custom-store"], verbs: ["get", "patch", "update"],
    }]);
    expect(coreRole.metadata?.namespace).toBe(core);
    expect(coreRole.rules).toEqual([{
      apiGroups: ["kars.azure.com"], resources: ["karsapprovals", "karstasks", "karsteams"], verbs: ["get", "list", "watch"],
    }]);
    const gatewayBindings = resources.filter((item) =>
      ["RoleBinding", "ClusterRoleBinding"].includes(item.kind!)
      && (item as V1RoleBinding).subjects?.some((subject) => subject.name === pod.serviceAccountName));
    expect(gatewayBindings).toHaveLength(2);
    for (const [name, targetNamespace] of [["kars-bridge-teams-gateway", namespace], [coreName, core]]) {
      const binding = resource<V1RoleBinding>(gatewayBindings, "RoleBinding", name!);
      expect(binding.metadata?.namespace).toBe(targetNamespace);
      expect(binding.roleRef).toEqual({ apiGroup: "rbac.authorization.k8s.io", kind: "Role", name });
      expect(binding.subjects).toEqual([{ kind: "ServiceAccount", name: pod.serviceAccountName, namespace }]);
      expect(binding.metadata?.labels?.["app.kubernetes.io/instance"]).toBe("kars-bridge");
      expect(binding.metadata?.annotations?.["helm.sh/resource-policy"]).toBeUndefined();
    }
    expect(resources.filter((item) => item.kind === "Namespace")).toEqual([]);
    if (namespace !== core) {
      expect(resources.filter((item) => item.metadata?.namespace === core)
        .map((item) => `${item.kind}/${item.metadata?.name}`).sort())
        .toEqual([`Role/${coreName}`, `RoleBinding/${coreName}`]);
    }
  });

  it("gives identically named releases in different add-on namespaces distinct core-read RBAC", () => {
    const roleNames = ["bridge-one", "bridge-two"].map((namespace) =>
      render("--set", `namespace=${namespace},core.namespace=kars-system`)
        .filter((item) => item.metadata?.namespace === "kars-system")
        .map((item) => `${item.kind}/${item.metadata?.name}`));
    expect(roleNames[0]).toHaveLength(2);
    expect(roleNames[1]).toHaveLength(2);
    expect(roleNames[0]!.some((name) => roleNames[1]!.includes(name))).toBe(false);
  });

  it("leaves all credential and Deployment mutation authority to core grants in both manifests", () => {
    const standalone = readFileSync(new URL("../../deploy/rbac.yaml", import.meta.url), "utf8")
      .split(/^---\s*$/m).map((doc) => loadYaml(doc) as KubernetesObject).filter(Boolean);
    for (const manifests of [render(), standalone]) {
      for (const object of manifests.filter((item) => ["Role", "ClusterRole"].includes(item.kind!))) {
        for (const rule of (object as V1Role).rules ?? []) {
          expect(rule.resources).not.toContain("secrets");
          if (rule.resources?.includes("deployments")) {
            expect(rule.verbs?.every((verb) => ["get", "list", "watch"].includes(verb))).toBe(true);
          }
          if (rule.resources?.includes("karscredentialgrants")) {
            expect(rule.verbs?.every((verb) => ["get", "list", "watch", "bridge-adapter"].includes(verb))).toBe(true);
          }
        }
      }
    }
  });

  it("separates the private add-on namespace from the configured core workspace", () => {
    const resources=render("--set","namespace=bridge-private,core.namespace=core-workspace,createNamespace=false");
    const bff=resource<V1Deployment>(resources,"Deployment","kars-bridge-bff");
    const env=bff.spec!.template.spec!.containers[0]!.env!;
    expect(bff.metadata?.namespace).toBe("bridge-private");
    expect(env.find((item)=>item.name==="BRIDGE_CORE_NAMESPACE")?.value).toBe("core-workspace");
    expect(env.find((item)=>item.name==="BRIDGE_INSTALL_NAMESPACE")?.value).toBe("bridge-private");
    const web=resource<V1Deployment>(resources,"Deployment","kars-bridge-web");
    expect(web.spec!.template.spec!.containers[0]!.env!.find((item)=>item.name==="BRIDGE_DEFAULT_NAMESPACE")?.value)
      .toBe("core-workspace");
  });
});
