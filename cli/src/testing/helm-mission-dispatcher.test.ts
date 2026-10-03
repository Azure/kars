// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
import { parseAllDocuments } from "yaml";
import { describe, expect, it } from "vitest";

const root = process.env.KARS_TEST_REPO_ROOT ?? fileURLToPath(new URL("../../../", import.meta.url));
function render(...settings: string[]): any[] {
  const args = ["template", "kars", resolve(root, "deploy/helm/kars"), "--namespace", "kars-system"];
  for (const setting of settings) args.push("--set", setting);
  return parseAllDocuments(execFileSync("helm", args, { encoding: "utf8", timeout: 30_000, stdio: ["ignore", "pipe", "pipe"] })).map(doc => doc.toJSON()).filter(Boolean);
}
const enabled = ["missionDispatcher.enabled=true", "missionDispatcher.authMode=development"];
const find = (docs: any[], kind: string, name: string) => docs.find(doc => doc.kind === kind && doc.metadata.name === name);

describe("Core durable mission dispatcher", () => {
  it("is absent by default with no controller dispatch opt-in", () => {
    const docs = render();
    expect(docs.some(doc => doc.metadata.name.includes("mission-dispatcher"))).toBe(false);
    const controller = find(docs, "Deployment", "kars-controller").spec.template.spec.containers[0];
    expect(controller.env.some((env: any) => env.name.startsWith("KARS_MISSION_"))).toBe(false);
  });
  it.each(["", "entra", "false"])("requires explicit development authentication, not %s", mode => {
    expect(() => render("missionDispatcher.enabled=true", `missionDispatcher.authMode=${mode}`)).toThrow();
  });
  it("provides a singleton hardened helper with controller-owned root reference", () => {
    const docs = render(...enabled); const deployment = find(docs, "Deployment", "kars-mission-dispatcher");
    expect(deployment.spec.replicas).toBe(1);
    expect(deployment.spec.strategy).toEqual({ type: "Recreate" });
    expect(deployment.metadata.labels["app.kubernetes.io/managed-by"]).toBe("Helm");
    expect(deployment.metadata.annotations).toEqual({ "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" });
    const pod = deployment.spec.template.spec;
    expect(pod.containers).toHaveLength(1);
    expect(pod.terminationGracePeriodSeconds).toBeGreaterThan(30);
    expect(pod.securityContext).toMatchObject({ runAsNonRoot: true, runAsUser: 1000, seccompProfile: { type: "RuntimeDefault" } });
    const container = pod.containers[0];
    expect(container.securityContext).toEqual({ allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: { drop: ["ALL"] } });
    expect(container.image).toBe("karsacr.azurecr.io/kars-mission-dispatcher:latest");
    expect(container.imagePullPolicy).toBe("Always");
    expect(container.readinessProbe.httpGet.path).toBe("/readyz");
    expect(container.livenessProbe.httpGet.path).toBe("/livez");
    const env = Object.fromEntries(container.env.map((item: any) => [item.name, item]));
    expect(env.KARS_MISSION_IDENTITY_ROOT).toEqual({ name: "KARS_MISSION_IDENTITY_ROOT", valueFrom: { secretKeyRef: { name: "kars-mission-dispatcher-identity", key: "root", optional: false } } });
    for (const [name, path] of [["POD_UID", "metadata.uid"], ["POD_NAME", "metadata.name"], ["POD_NAMESPACE", "metadata.namespace"]]) expect(env[name].valueFrom.fieldRef.fieldPath).toBe(path);
    expect(env.AGENTMESH_RELAY_URL.value).toBe("ws://agentmesh-relay.agentmesh.svc.cluster.local:8765");
    expect(env.AGENTMESH_REGISTRY_URL.value).toBe("http://agentmesh-registry.agentmesh.svc.cluster.local:8080");
    expect(docs.some(doc => doc.kind === "Secret" && doc.metadata.name === "kars-mission-dispatcher-identity")).toBe(false);
    expect(docs.some(doc => ["Service", "Ingress"].includes(doc.kind) && doc.metadata.name === "kars-mission-dispatcher")).toBe(false);
  });
  it("limits Secret access to GET of its own named root", () => {
    const docs = render(...enabled);
    expect(find(docs, "Role", "kars-mission-dispatcher-root").rules).toEqual([{ apiGroups: [""], resources: ["secrets"], resourceNames: ["kars-mission-dispatcher-identity"], verbs: ["get"] }]);
    const rules = find(docs, "ClusterRole", "kars-kars-system-mission-dispatcher").rules;
    expect(rules.some((rule: any) => rule.resources.includes("secrets") || rule.resources.includes("*") || rule.verbs.includes("*"))).toBe(false);
    expect(rules.some((rule: any) => rule.resources.some((resource: string) => resource.includes("exec") || resource.includes("token")))).toBe(false);
    expect(rules.find((rule: any) => rule.resources.includes("karstasks")).verbs).toEqual(["get", "list", "patch"]);
    expect(rules.find((rule: any) => rule.resources.includes("configmaps")).verbs).toEqual(["get", "create", "update", "patch"]);
  });
  it("wires the same helper/release and ReplicaSet reads into the controller", () => {
    const docs = render(...enabled);
    const env = find(docs, "Deployment", "kars-controller").spec.template.spec.containers[0].env;
    expect(env).toEqual(expect.arrayContaining([
      { name: "KARS_MISSION_DISPATCH_ENABLED", value: "true" },
      { name: "KARS_MISSION_DISPATCHER_DEPLOYMENT", value: "kars-mission-dispatcher" },
      { name: "KARS_MISSION_DISPATCHER_RELEASE", value: "kars" },
    ]));
    const role = docs.find(doc => doc.kind === "ClusterRole" && doc.rules.some((rule: any) => rule.resources.includes("karssandboxes") && rule.verbs.includes("watch")));
    expect(role.rules).toContainEqual({ apiGroups: ["apps"], resources: ["replicasets"], verbs: ["get", "list"] });
  });
  it("supports content-addressed image installation and an external official mesh", () => {
    const digest = `sha256:${"a".repeat(64)}`;
    const docs = render(...enabled, `missionDispatcher.image.digest=${digest}`, "missionDispatcher.relayUrl=wss://mesh.example/relay", "missionDispatcher.registryUrl=https://mesh.example/registry");
    const container = find(docs, "Deployment", "kars-mission-dispatcher").spec.template.spec.containers[0];
    expect(container.image).toBe(`karsacr.azurecr.io/kars-mission-dispatcher@${digest}`);
    expect(container.env).toEqual(expect.arrayContaining([{ name: "AGENTMESH_RELAY_URL", value: "wss://mesh.example/relay" }, { name: "AGENTMESH_REGISTRY_URL", value: "https://mesh.example/registry" }]));
  });
  it("rejects a malformed image digest", () => {
    expect(() => render(...enabled, "missionDispatcher.image.digest=latest")).toThrow();
  });
});
