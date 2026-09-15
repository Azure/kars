// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { createHash } from "node:crypto";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import { continuityFixture, rootPod } from "./private-activation-fixtures.js";
import {
  PRIVATE_PREFIX as P, matchesReviewedExecution, previewPrivateActivation, templateDigest, validatePrivateActivation, validateQualifiedActivation,
  type Execute,
} from "./private-activation.js";

const client = "11111111-1111-1111-1111-111111111111";
const tenant = "22222222-2222-2222-2222-222222222222";
const wiVolume = "azure-identity-token";
const wiPath = "/var/run/secrets/azure/tokens";
const history = `${P}root-retirement`;
const revisionKey = "kars.azure.com/observation-privacy-revision";
const controllerKey = "kars.azure.com/privacy-controller-uid";
const namespaceKey = "kars.azure.com/privacy-namespace-uid";

function admitted(input: any, suffix = "live1") {
  const pod = structuredClone(input);
  if (pod.spec.nodeName) {
    pod.metadata.labels["topology.kubernetes.io/region"] = "reviewed-region";
    pod.metadata.labels["topology.kubernetes.io/zone"] = "reviewed-zone";
  }
  pod.spec.serviceAccount = "kars-controller";
  pod.spec.enableServiceLinks = true;
  pod.spec.priority = 0;
  pod.spec.preemptionPolicy = "PreemptLowerPriority";
  pod.spec.imagePullSecrets = [{ name: "acr-pull" }];
  pod.spec.volumes = [
    ...(pod.spec.volumes ?? []),
    { name: `kube-api-access-${suffix}`, projected: { defaultMode: 420, sources: [
      { serviceAccountToken: { expirationSeconds: 3607, path: "token" } },
      { configMap: { name: "kube-root-ca.crt", items: [{ key: "ca.crt", path: "ca.crt" }] } },
      { downwardAPI: { items: [{ path: "namespace", fieldRef: { apiVersion: "v1", fieldPath: "metadata.namespace" } }] } },
    ] } },
    { name: wiVolume, projected: { defaultMode: 420, sources: [
      { serviceAccountToken: { audience: "api://AzureADTokenExchange", expirationSeconds: 3600, path: wiVolume } },
    ] } },
  ];
  for (const container of [...pod.spec.containers, ...(pod.spec.initContainers ?? [])]) {
    container.env = [...(container.env ?? []),
      ...[{ name: "AZURE_CLIENT_ID", value: client }, { name: "AZURE_TENANT_ID", value: tenant },
        { name: "AZURE_FEDERATED_TOKEN_FILE", value: `${wiPath}/${wiVolume}` },
        { name: "AZURE_AUTHORITY_HOST", value: "https://login.microsoftonline.com/" },
      ].filter(entry => !(container.env ?? []).some((existing: any) => existing.name === entry.name)),
    ];
    container.volumeMounts = [...(container.volumeMounts ?? []),
      { name: `kube-api-access-${suffix}`, readOnly: true, mountPath: "/var/run/secrets/kubernetes.io/serviceaccount" },
      { name: wiVolume, readOnly: true, mountPath: wiPath },
    ];
  }
  pod.spec.tolerations = [
    ...(pod.spec.tolerations ?? []),
    ...["node.kubernetes.io/not-ready", "node.kubernetes.io/unreachable"].map(key =>
      ({ key, operator: "Exists", effect: "NoExecute", tolerationSeconds: 300 })),
    { key: "node.kubernetes.io/memory-pressure", operator: "Exists", effect: "NoSchedule" },
  ];
  return pod;
}

function setup(configure?: (template: any) => void) {
  const f = continuityFixture();
  const account = f.objects.get(f.key("serviceaccount", "kars-controller", "core"));
  account.metadata.annotations = { "azure.workload.identity/client-id": client };
  account.imagePullSecrets = [{ name: "acr-pull" }];
  const template = f.deployment.spec.template as any;
  template.metadata.labels = { app: "controller", "azure.workload.identity/use": "true",
    "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "controller" };
  Object.assign(template.spec, {
    automountServiceAccountToken: true, securityContext: { runAsNonRoot: true },
    dnsPolicy: "ClusterFirst", restartPolicy: "Always", schedulerName: "default-scheduler",
    terminationGracePeriodSeconds: 30, nodeSelector: { agentpool: "gpuh100" },
  });
  Object.assign(template.spec.containers[0], {
    env: [{ name: "AZURE_CLIENT_ID", value: client }, { name: "RUST_LOG", value: "info" },
      { name: "KARS_OBSERVATION_PRIVACY_RPC_ENABLED", value: "true" }],
    resources: { requests: { cpu: "100m", memory: "128Mi" }, limits: { cpu: "1", memory: "256Mi" } },
    securityContext: { allowPrivilegeEscalation: false, readOnlyRootFilesystem: true },
  });
  configure?.(template);
  const endpoint = JSON.stringify({ capability: "kars.azure.com/observation-privacy/v1", namespace: "core",
    namespaceUid: "core-uid", controllerUid: "controller-sa", serviceUid: "privacy-service",
    port: 9448, descriptorUid: "privacy-descriptor", tlsUid: "privacy-tls", tlsVersion: "1",
    serverName: "privacy-core-uid.kars.internal", caPem: "-----BEGIN CERTIFICATE-----\nPUBLIC_TEST_FIXTURE", expiresAt: 2_000_000_000 });
  const revision = createHash("sha256").update(endpoint).digest("hex").slice(0, 32);
  for (const [kind, uid] of [["configmap", "privacy-descriptor"], ["service", "privacy-service"]]) {
    f.objects.set(f.key(kind!, "kars-observation-privacy", "core"), {
      metadata: { name: "kars-observation-privacy", namespace: "core", uid, resourceVersion: "1",
        ...(kind === "configmap" ? { annotations: { [controllerKey]: "controller-sa", [namespaceKey]: "core-uid" } } : {}) },
      ...(kind === "configmap" ? { data: { "config.json": endpoint } } : { spec: { selector: {
        "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "controller", [revisionKey]: revision,
      } } }),
    });
  }
  const freshPod = (uid: string) => {
    const pod = rootPod(f, uid) as any;
    const rs = f.objects.get(f.key("replicasets.apps", "root-rs", "core"));
    const hash = uid === "old-root" ? "7bc99bcde" : "6acd998f3";
    rs.metadata.name = `kars-controller-${hash}`;
    rs.metadata.uid = uid === "old-root" ? "old-rs-uid" : "new-rs-uid";
    rs.metadata.ownerReferences[0].blockOwnerDeletion = true;
    rs.spec.template.metadata.labels["pod-template-hash"] = hash;
    rs.spec.selector = { matchLabels: { app: "controller", "pod-template-hash": hash } };
    f.objects.delete(f.key("replicasets.apps", "root-rs", "core"));
    f.objects.set(f.key("replicasets.apps", rs.metadata.name, "core"), rs);
    pod.metadata.labels = structuredClone(rs.spec.template.metadata.labels);
    pod.metadata.ownerReferences[0].name = rs.metadata.name;
    pod.metadata.ownerReferences[0].uid = rs.metadata.uid;
    pod.metadata.ownerReferences[0].blockOwnerDeletion = true;
    pod.spec.nodeName = "aks-node";
    const value = admitted(pod);
    value.metadata.labels[revisionKey] = revision;
    value.metadata.annotations[controllerKey] = "controller-sa";
    value.metadata.annotations[namespaceKey] = "core-uid";
    return value;
  };
  f.pods.set("core", [freshPod("old-root")]);
  const controls: {
    response?: (pod: any) => any; duringDryRun?: () => void; restored?: (pod: any) => void;
    error?: unknown; failRestored?: boolean; pending?: (pod: any) => void;
  } = {};
  const requests: any[] = [];
  const execute: Execute = async (args, input) => {
    if (args[0] === "create" && args[1] === "--raw") {
      f.calls.push(args);
      expect(args).toEqual(["create", "--raw",
        "/api/v1/namespaces/core/pods?dryRun=All&fieldValidation=Strict",
        "--request-timeout=30s", "-f", "-"]);
      const request = JSON.parse(input!);
      requests.push(request);
      expect(request.kind).toBe("Pod");
      expect(request.metadata.name).not.toBe(f.pods.get("core")![0].metadata.name);
      expect(request.metadata.uid).toBeUndefined();
      expect(request.metadata.resourceVersion).toBeUndefined();
      expect(request.spec).toEqual({ ...template.spec, nodeName: "aks-node" });
      if (controls.error) throw controls.error;
      if (controls.failRestored && f.pods.get("core")![0].metadata.uid === "new-root") {
        throw { stderr: "Error from server (Forbidden): PRIVATE_SECRET_DO_NOT_PRINT", exitCode: 1 };
      }
      const response = admitted(request, "dry12");
      response.metadata.uid = "dry-run-only";
      controls.duringDryRun?.();
      return JSON.stringify(controls.response ? controls.response(response) : response);
    }
    if (args[0] === "get" && args[1] === "pods" && args[2] !== "-n") {
      f.calls.push(args);
      const pod = f.pods.get("core")!.find(value => value.metadata.name === args[2]);
      if (!pod) throw new Error("Pod no longer exists");
      return JSON.stringify(pod);
    }
    const result = await f.execute(args, input);
    if (args[0] === "patch" && args[1] === "namespace" && args[2] === "core") {
      const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
      if (patch.metadata.annotations?.[`${P}state`] === "Pending") controls.pending?.(f.pods.get("core")![0]);
    }
    if (args[0] === "patch" && args[2] === "kars-controller") {
      const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
      if (patch.spec?.replicas > 0) {
        const pod = freshPod("new-root");
        controls.restored?.(pod);
        f.pods.set("core", [pod]);
      }
    }
    return result;
  };
  const preview = (namespace = "work", consumers: string[] = []) =>
    previewPrivateActivation(execute, namespace, [{ namespace: "reader" }], [], "core", "service-accounts", consumers);
  const document = async (namespace = "work") => ({
    apiVersion: "kars.azure.com/v1alpha1", kind: "KarsCredentialGrant",
    metadata: { name: "workspace", namespace },
    spec: { workspaceUid: `${namespace}-uid`, enabled: true,
      writers: [{ namespace: "reader", name: "bff", uid: "reader-sa" }], privateActivation: await preview(namespace) },
  });
  const pod = () => f.pods.get("core")![0];
  const readOnly = () => f.calls.every(args => ["get", "auth"].includes(args[0]!)
    || (args[0] === "create" && args[1] === "--raw" && args[2]!.includes("?dryRun=All&")));
  return { ...f, execute, preview, document, requests, controls, pod, account, readOnly };
}

describe("AKS admitted private root consumers", () => {
  it("previews and applies the original Deployment-only scope through retirement, replacement and second-workspace continuity", async () => {
    const f = setup();
    const original = f.preserved();
    const document = await f.document();
    expect(f.readOnly()).toBe(true);
    expect(f.preserved()).toEqual(original);
    expect(f.requests).toHaveLength(1);
    const root = document.spec.privateActivation.namespaces.find(scope => scope.namespace.name === "core")!;
    expect(root.consumers.map(consumer => consumer.kind)).toEqual(["Deployment"]);
    await applyReviewedGrant(f.execute, document);
    expect(f.deployment.spec.replicas).toBe(1);
    expect(f.pod().metadata.uid).toBe("new-root");
    expect(f.pod().metadata.ownerReferences[0].uid).toBe("new-rs-uid");
    const proof = JSON.parse(f.namespace("core").metadata.annotations[history]);
    expect(proof.version).toBe(2);
    expect(JSON.parse(proof.retirement).captured["core-uid"]).toEqual(["old-root"]);
    expect(templateDigest(f.deployment)).toBe(document.spec.privateActivation.root.templateDigest);
    expect(f.grant().spec.privateActivation.phase).toBe("qualified");
    expect(f.calls.some(args => ["nodes", "node", "secret", "events", "exec"].includes(args[1]!))).toBe(false);
    expect(JSON.stringify(document)).not.toContain("PUBLIC_TEST_FIXTURE");
    expect(JSON.stringify(document)).not.toContain("AZURE_CLIENT_ID");
    expect(f.requests.length).toBeLessThan(20);
    const first = f.preserved();
    f.calls.length = 0;
    await applyReviewedGrant(f.execute, await f.document("second"));
    expect(f.preserved()).toEqual(first);
    expect(f.calls.filter(args => args[0] === "patch").every(args => args[1] === "namespace" && args[2] === "second")).toBe(true);
    expect(f.authority.size).toBe(2);
    await validateQualifiedActivation(f.execute, f.grant().spec.privateActivation);
    await validateQualifiedActivation(f.execute, f.grant("second").spec.privateActivation);
  });

  it("resumes an original restoring/no-grant attempt without changing its binding, epochs, replica intent or Pod UID", async () => {
    const f = setup();
    const document = await f.document();
    f.controls.failRestored = true;
    await expect(applyReviewedGrant(f.execute, document)).rejects.toThrow("Forbidden");
    expect(f.grant()).toBeUndefined();
    expect(f.deployment.spec.replicas).toBe(1);
    const retirement = f.namespace("core").metadata.annotations[history];
    expect(JSON.parse(retirement).phase).toBe("restoring");
    const pod = structuredClone(f.pod());
    const epoch = f.namespace("core").metadata.annotations[`${P}epoch`];
    f.controls.failRestored = false;
    f.calls.length = 0;
    await expect(f.preview("second")).rejects.toThrow("original exact review");
    await expect(f.preview("work", [`core/Pod/${pod.metadata.name}`])).rejects.toThrow();
    await applyReviewedGrant(f.execute, await f.document());
    expect(JSON.parse(f.namespace("core").metadata.annotations[history]).retirement).toBe(retirement);
    expect(f.namespace("core").metadata.annotations[`${P}epoch`]).toBe(epoch);
    expect(f.pod()).toEqual(pod);
    expect(f.calls.some(args => args[0] === "patch" && args[2] === "kars-controller")).toBe(false);
    await applyReviewedGrant(f.execute, await f.document("second"));
  });

  it("keeps explicitly reviewed Pod semantics without silently changing the Deployment-only scope", async () => {
    const f = setup();
    f.pod().spec.containers[0].env.push({ name: "EXPLICIT_REVIEW", value: "reviewed" });
    const review = await f.preview("work", ["core/Pod/old-root"]);
    expect(review.namespaces.find(scope => scope.namespace.name === "core")!.consumers.map(c => c.kind)).toEqual(["Pod", "Deployment"]);
    expect(f.requests).toHaveLength(0);
    f.pod().spec.containers[0].env.at(-1).value = "changed";
    await expect(validatePrivateActivation(f.execute, review)).rejects.toThrow("template changed");
    expect(f.readOnly()).toBe(true);
  });

  it("preserves a reviewed client env when the ServiceAccount has no client-ID annotation, as on H100", async () => {
    const f = setup();
    delete f.account.metadata.annotations["azure.workload.identity/client-id"];
    await applyReviewedGrant(f.execute, await f.document());
    expect(f.grant().spec.privateActivation.phase).toBe("qualified");
    expect(f.pod().spec.containers[0].env.find((e: any) => e.name === "AZURE_CLIENT_ID")).toEqual({ name: "AZURE_CLIENT_ID", value: client });
  });

  it("sends all reviewed execution fields verbatim and rejects server pruning of an unknown field", async () => {
    const f = setup(template => {
      template.spec.containers[0].envFrom = [{ configMapRef: { name: "reviewed-config" } }];
      template.spec.containers[0].env.push({ name: "REVIEWED_REF", valueFrom: { secretKeyRef: { name: "reviewed-key", key: "key" } } });
      template.spec.containers[0].lifecycle = { preStop: { exec: { command: ["controller", "--stop"] } } };
      template.spec.futureRuntimeSetting = { security: "must-survive" };
    });
    f.controls.response = pod => {
      delete pod.spec.futureRuntimeSetting;
      return pod;
    };
    await expect(f.preview()).rejects.toThrow();
    expect(f.requests).toHaveLength(1);
    expect(f.requests[0].spec.futureRuntimeSetting).toEqual({ security: "must-survive" });
    expect(f.requests[0].spec.containers[0].envFrom).toEqual([{ configMapRef: { name: "reviewed-config" } }]);
    expect(f.requests[0].spec.containers[0].lifecycle.preStop.exec.command).toEqual(["controller", "--stop"]);
    expect(f.readOnly()).toBe(true);
  });

  it("keeps the post-staging race fence when a Pod changes after successful preflight", async () => {
    const f = setup();
    const document = await f.document();
    f.controls.pending = pod => { pod.spec.containers[0].image = "unreviewed-after-preflight"; };
    await expect(applyReviewedGrant(f.execute, document)).rejects.toThrow();
    expect(f.namespace("core").metadata.annotations[`${P}state`]).toBe("Pending");
    expect(f.namespace("core").metadata.annotations[`${P}epoch`]).toBeUndefined();
    expect(f.deployment.spec.replicas).toBe(1);
    expect(f.grant()).toBeUndefined();
  });

  const mutations: [string, (pod: any) => void][] = [
    ["WI audience", p => p.spec.volumes[1].projected.sources[0].serviceAccountToken.audience = "other"],
    ["WI token path", p => p.spec.volumes[1].projected.sources[0].serviceAccountToken.path = "other"],
    ["WI expiration", p => p.spec.volumes[1].projected.sources[0].serviceAccountToken.expirationSeconds = 7200],
    ["WI extra source", p => p.spec.volumes[1].projected.sources.push({ secret: { name: "router-private-key" } })],
    ["WI mount", p => p.spec.containers[0].volumeMounts[1].readOnly = false],
    ["client ID", p => p.spec.containers[0].env[0].value = tenant],
    ["provider env", p => p.spec.containers[0].env[1].value = "different"],
    ["token environment", p => p.spec.containers[0].env.find((e: any) => e.name === "AZURE_FEDERATED_TOKEN_FILE").value = "/other"],
    ["authority host", p => p.spec.containers[0].env.find((e: any) => e.name === "AZURE_AUTHORITY_HOST").value = "https://untrusted.example/"],
    ["duplicate Azure env", p => p.spec.containers[0].env.push({ name: "AZURE_CLIENT_ID", value: client })],
    ["envFrom", p => p.spec.containers[0].envFrom = [{ secretRef: { name: "router-private-key" } }]],
    ["private key ref", p => p.spec.containers[0].env.push({ name: "KEY", valueFrom: { secretKeyRef: { name: "router-private-key", key: "key" } } })],
    ["extra volume", p => p.spec.volumes.push({ name: "host", hostPath: { path: "/" } })],
    ["API token audience", p => p.spec.volumes[0].projected.sources[0].serviceAccountToken.audience = "other"],
    ["API token expiry", p => p.spec.volumes[0].projected.sources[0].serviceAccountToken.expirationSeconds = 86400],
    ["API token permissions", p => p.spec.volumes[0].projected.defaultMode = 511],
    ["API mount subPath", p => p.spec.containers[0].volumeMounts[0].subPath = "token"],
    ["image pull secret", p => p.spec.imagePullSecrets.push({ name: "unreviewed" })],
    ["memory toleration", p => p.spec.tolerations[2].effect = "NoExecute"],
    ["unbounded toleration", p => delete p.spec.tolerations[0].tolerationSeconds],
    ["duplicate toleration", p => p.spec.tolerations.push(p.spec.tolerations[0])],
    ["extra toleration", p => p.spec.tolerations.push({ operator: "Exists" })],
    ["host network", p => p.spec.hostNetwork = true],
    ["host PID", p => p.spec.hostPID = true],
    ["privilege", p => p.spec.containers[0].securityContext.privileged = true],
    ["image", p => p.spec.containers[0].image = "different"],
    ["command", p => p.spec.containers[0].command = ["other"]],
    ["sidecar", p => p.spec.containers.push({ name: "sidecar", image: "other" })],
    ["init container", p => p.spec.initContainers = [{ name: "init", image: "other" }]],
    ["ephemeral container", p => p.spec.ephemeralContainers = [{ name: "debug", image: "other" }]],
    ["lifecycle hook", p => p.spec.containers[0].lifecycle = { postStart: { exec: { command: ["other"] } } }],
    ["service links", p => p.spec.enableServiceLinks = false],
    ["priority", p => p.spec.priority = 42],
    ["preemption", p => p.spec.preemptionPolicy = "Never"],
    ["node selector", p => p.spec.nodeSelector = { agentpool: "other" }],
    ["runtime class", p => p.spec.runtimeClassName = "other"],
    ["unknown runtime field", p => p.spec.newSecurityField = "unreviewed"],
    ["service account", p => p.spec.serviceAccountName = "other"],
    ["network label", p => p.metadata.labels.app = "other"],
    ["network annotation", p => p.metadata.annotations["k8s.v1.cni.cncf.io/networks"] = "other"],
    ["webhook marker", p => p.metadata.annotations["azure.workload.identity/injected"] = "true"],
    ["topology zone", p => p.metadata.labels["topology.kubernetes.io/zone"] = "unreviewed"],
    ["topology omission", p => delete p.metadata.labels["topology.kubernetes.io/zone"]],
    ["topology extra label", p => p.metadata.labels["topology.kubernetes.io/other"] = "unreviewed"],
    ["privacy controller UID", p => p.metadata.annotations[controllerKey] = "other"],
    ["privacy namespace UID", p => p.metadata.annotations[namespaceKey] = "other"],
    ["privacy revision", p => p.metadata.labels[revisionKey] = "a".repeat(32)],
    ["privacy missing annotation", p => delete p.metadata.annotations[controllerKey]],
    ["extra owner", p => p.metadata.ownerReferences.push({ apiVersion: "v1", kind: "Pod", name: "other", uid: "other" })],
    ["owner UID", p => p.metadata.ownerReferences[0].uid = "other"],
    ["owner API", p => p.metadata.ownerReferences[0].apiVersion = "other/v1"],
    ["namespace", p => p.metadata.namespace = "other"],
    ["missing admission", p => {
      p.spec = { serviceAccountName: "kars-controller", containers: [{ name: "controller", image: "fixture", command: ["controller"] }] };
    }],
  ];
  it("still requires actual root Workload Identity admission when ordinary execution matches", async () => {
    const f = setup();
    const pod = f.pod();
    const parent = f.objects.get(f.key("replicasets.apps", pod.metadata.ownerReferences[0].name, "core"));
    pod.spec = { ...structuredClone(parent.spec.template.spec), nodeName: "aks-node" };
    expect(matchesReviewedExecution(pod.spec, parent.spec.template.spec, true)).toBe(true);
    const before = f.preserved();
    await expect(f.preview()).rejects.toThrow("Pod admission could not be verified");
    expect(f.preserved()).toEqual(before);
    expect(f.readOnly()).toBe(true);
    expect(f.grant()).toBeUndefined();
  });

  it.each(mutations)("rejects unexplained %s before protective writes in preview and actual apply", async (_name, mutate) => {
    const f = setup();
    const document = await f.document();
    mutate(f.pod());
    const before = f.preserved();
    f.calls.length = 0;
    await expect(f.preview()).rejects.toThrow();
    await expect(applyReviewedGrant(f.execute, document)).rejects.toThrow();
    expect(f.preserved()).toEqual(before);
    expect(f.readOnly()).toBe(true);
    expect(f.namespace("core").metadata.annotations[history]).toBeUndefined();
    expect(f.grant()).toBeUndefined();
  });

  it.each(["namespace", "account", "pod", "replicaset", "deployment"])("fences %s UID/resourceVersion drift across the dry-run", async kind => {
    for (const field of ["uid", "resourceVersion"]) {
      const f = setup();
      f.controls.duringDryRun = () => {
        const object = kind === "namespace" ? f.namespace("core") : kind === "account" ? f.account
          : kind === "pod" ? f.pod() : kind === "deployment" ? f.deployment
            : f.objects.get(f.key("replicasets.apps", f.pod().metadata.ownerReferences[0].name, "core"));
        object.metadata[field] = "replaced";
      };
      await expect(f.preview()).rejects.toThrow();
      expect(f.requests).toHaveLength(1);
      expect(f.readOnly()).toBe(true);
    }
  });

  it.each(["configmap", "service"])("fences publication %s UID, resourceVersion and content drift", async kind => {
    for (const change of ["uid", "resourceVersion", "content"]) {
      const f = setup();
      f.controls.duringDryRun = () => {
        const object = f.objects.get(f.key(kind, "kars-observation-privacy", "core"));
        if (change !== "content") object.metadata[change] = "other";
        else if (kind === "configmap") object.data["config.json"] += " ";
        else object.spec.selector[revisionKey] = "other";
      };
      await expect(f.preview()).rejects.toThrow();
      expect(f.readOnly()).toBe(true);
    }
  });

  it.each(["descriptor UID", "descriptor identity", "descriptor revision", "service UID", "service selector", "disabled RPC"])("rejects unproven %s publication metadata", async fault => {
    const f = setup();
    if (fault === "disabled RPC") {
      (f.deployment.spec.template.spec.containers[0] as any).env.find((e: any) => e.name === "KARS_OBSERVATION_PRIVACY_RPC_ENABLED").value = "false";
    } else {
      const object = f.objects.get(f.key(fault.startsWith("service") ? "service" : "configmap", "kars-observation-privacy", "core"));
      if (fault.endsWith("UID")) object.metadata.uid = "replaced";
      if (fault === "descriptor identity") object.metadata.annotations[controllerKey] = "other";
      if (fault === "descriptor revision") object.data["config.json"] += " ";
      if (fault === "service selector") object.spec.selector.app = "other";
    }
    await expect(f.preview()).rejects.toThrow();
    expect(f.readOnly()).toBe(true);
  });

  it.each(["expired", "unknown field", "foreign identity"])("rejects a %s RPC descriptor even if its digest and Pod/Service labels agree", async fault => {
    const f = setup();
    const descriptor = f.objects.get(f.key("configmap", "kars-observation-privacy", "core"));
    const endpoint = JSON.parse(descriptor.data["config.json"]);
    if (fault === "expired") endpoint.expiresAt = 0;
    if (fault === "unknown field") endpoint.unreviewed = "other";
    if (fault === "foreign identity") endpoint.controllerUid = "other";
    descriptor.data["config.json"] = JSON.stringify(endpoint);
    const revision = createHash("sha256").update(descriptor.data["config.json"]).digest("hex").slice(0, 32);
    f.pod().metadata.labels[revisionKey] = revision;
    f.objects.get(f.key("service", "kars-observation-privacy", "core")).spec.selector[revisionKey] = revision;
    await expect(f.preview()).rejects.toThrow();
    expect(f.readOnly()).toBe(true);
  });

  it.each(["owner", "template", "namespace metadata", "SA annotation", "SA pull secrets"])("fences %s changes even without a fixture resourceVersion bump", async fault => {
    const f = setup();
    f.controls.duringDryRun = () => {
      if (fault === "owner") f.pod().metadata.ownerReferences[0].uid = "replaced";
      if (fault === "template") (f.deployment.spec.template.spec.containers[0] as any).image = "other";
      if (fault === "namespace metadata") f.namespace("core").metadata.labels = { admission: "different" };
      if (fault === "SA annotation") f.account.metadata.annotations["azure.workload.identity/client-id"] = tenant;
      if (fault === "SA pull secrets") f.account.imagePullSecrets = [{ name: "other" }];
    };
    await expect(f.preview()).rejects.toThrow();
    expect(f.readOnly()).toBe(true);
  });

  it.each(["sidecar", "unknown runtime field", "envFrom", "WI audience", "network annotation"])("rejects dry-run %s mutation rather than trusting any webhook", async name => {
    const f = setup();
    f.controls.response = pod => {
      mutations.find(([key]) => key === name)![1](pod);
      return pod;
    };
    await expect(f.preview()).rejects.toThrow();
    expect(f.requests).toHaveLength(1);
    expect(f.readOnly()).toBe(true);
  });

  it.each(["namespace", "kind", "name", "persisted", "missing spec", "pruned field", "tenant"])("rejects invalid dry-run %s evidence", async fault => {
    const f = setup();
    f.controls.response = pod => {
      if (fault === "namespace") pod.metadata.namespace = "other";
      if (fault === "kind") pod.kind = "Deployment";
      if (fault === "name") pod.metadata.name = f.pod().metadata.name;
      if (fault === "persisted") pod.metadata.resourceVersion = "1";
      if (fault === "missing spec") delete pod.spec;
      if (fault === "pruned field") delete pod.spec.containers[0].securityContext;
      if (fault === "tenant") pod.spec.containers[0].env.find((e: any) => e.name === "AZURE_TENANT_ID").value = client;
      return pod;
    };
    await expect(f.preview()).rejects.toThrow();
    expect(f.readOnly()).toBe(true);
  });

  it.each(["Forbidden", "Invalid", "unknown", "unsupported side effects"])("does not retry %s and redacts admission command failures", async reason => {
    const f = setup();
    f.controls.error = { stderr: `Error from server (${reason}): PRIVATE_SECRET_DO_NOT_PRINT`, exitCode: 1,
      message: "PRIVATE_SECRET_DO_NOT_PRINT", stdout: "PRIVATE_SECRET_DO_NOT_PRINT" };
    const error = await f.preview().catch(error => error);
    expect(String(error)).toContain("KARS_PRIVATE_COMMAND_FAILURE");
    expect(String(error)).not.toContain("PRIVATE_SECRET_DO_NOT_PRINT");
    expect(JSON.stringify(error)).not.toContain("PRIVATE_SECRET_DO_NOT_PRINT");
    expect(f.requests).toHaveLength(1);
    expect(f.readOnly()).toBe(true);
    expect(f.namespace("core").metadata.annotations[history]).toBeUndefined();
  });

  it.each(["lifecycle hook", "service account"])("rejects replacement %s drift after restoration without publishing a grant or changing the frozen scope", async fault => {
    const f = setup();
    const document = await f.document();
    f.controls.restored = mutations.find(([name]) => name === fault)![1];
    await expect(applyReviewedGrant(f.execute, document)).rejects.toThrow();
    expect(f.grant()).toBeUndefined();
    expect(f.deployment.spec.replicas).toBe(1);
    const saved = f.namespace("core").metadata.annotations[history];
    expect(JSON.parse(saved).phase).toBe("restoring");
    f.calls.length = 0;
    await expect(f.preview()).rejects.toThrow();
    expect(f.namespace("core").metadata.annotations[history]).toBe(saved);
    expect(f.readOnly()).toBe(true);
  });

  it("rejects second-workspace continuity on changed root execution while preserving the first grant", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    f.pod().spec.containers[0].image = "unreviewed";
    const original = f.preserved();
    f.calls.length = 0;
    await expect(f.document("second")).rejects.toThrow();
    expect(f.grant("second")).toBeUndefined();
    expect(f.preserved()).toEqual(original);
    expect(f.readOnly()).toBe(true);
  });
});
