// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";
import { annotations, canonical, consumesPrivateAuthority, templateDigest } from "../lib/private-activation.js";
import { retirementBinding } from "../lib/private-activation-retirement.js";
import { fixture, adoptedFixture, hash, root, render } from "./sre-first-install-transition.test-support.js";

describe("receipt-bound private Core continuity", () => {
  const retirementKey = "kars.azure.com/private-root-retirement";
  const publicationKey = "kars.azure.com/private-core-publication";
  function privateFixture(qualified = false, replicas = 1, managedTls = false): any {
    const f = fixture();
    f.root.Values.controller.replicas = replicas;
    f.root.Values.evals = { enabled: true, runnerImage: "ghcr.io/azure/kars-conformance-runner:latest" };
    Object.assign(f.root.Values.inferenceBudget, { tlsSecretName: "budget-tls", caBundle: "original-public-ca" });
    if (managedTls) f.root.Values.inferenceBudget.tls = { certificate: "test-certificate", privateKey: "test-key" };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap = structuredClone(adoptedFixture().adoptionMap);
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.budget = structuredClone(f.root.Values.inferenceBudget);
    delete intent.budget.tls;
    intent.valuesDigest = hash(canonicalSchema({ ...f.root.Values, schemaHook: { enabled: true } }));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    const identity = (name: string, uid: string, resourceVersion = "10") => ({ name, uid, resourceVersion });
    const ns = identity("kars-system", "workspace-uid");
    f.controller = { metadata: { ...identity("kars-controller", "controller-uid"), namespace: ns.name }, spec: {
      replicas: qualified ? replicas : 0,
      template: { metadata: { annotations: { "kars.azure.com/private-epoch": "current-epoch", "test/encoding": "<bound>&\u2028" } },
        spec: { serviceAccountName: "kars-controller", containers: [{ name: "controller", image: "registry.example/controller:latest@sha256:" + "a".repeat(64) }] } },
    } };
    const definition = JSON.parse(readFileSync(`${root}deploy/helm/kars/files/private-consumption.json`, "utf8"));
    f.privateDefinition = { controllers: definition.controllers, secrets: definition.secrets, tokenAudiences: definition.tokenAudiences };
    f.privateControllers = definition.controllers;
    const activation: any = { contract: "kars.azure.com/private-consumption/v1", phase: qualified ? "qualified" : "reviewed",
      bundleRevision: "b".repeat(64), profile: "service-accounts", controllerUids: Object.fromEntries(f.privateControllers.map((name: string) => [name, `uid-${name}`])),
      root: { namespace: ns, deployment: identity("kars-controller", "controller-uid"), account: identity("kars-controller", "root-account-uid"),
        replicaIntent: replicas, templateDigest: templateDigest(f.controller),
        budgetTls: { namespace: ns, secret: identity("budget-tls", "tls-uid", qualified ? "12" : "10"), keyDigest: (qualified ? "c" : "a").repeat(64) } },
      namespaces: [{ namespace: ns, ...(qualified ? { epoch: "current-epoch" } : {}), consumers: [{ kind: "Deployment", object: identity("kars-controller", "controller-uid"), templateDigest: templateDigest(f.controller) }] }],
    };
    const state = { version: 1, attempt: "d".repeat(64), binding: retirementBinding(activation), replicaIntent: replicas,
      originalVersion: "10", pauseRoot: true, phase: qualified ? "restoring" : "retired", captured: { [ns.uid]: ["retired-pod-uid"] },
      exposedKeys: ["a".repeat(64)], baseline: { secretUid: "tls-uid", resourceVersion: "11", keyDigest: "a".repeat(64) } };
    const proof = qualified ? { version: 2, activation, retirement: canonical(state), retirementDigest: hash(canonical(state)) } : state;
    f.workspace = { metadata: { ...ns, annotations: { ...annotations(activation, activation.namespaces[0], qualified ? "Qualified" : "Pending"),
      [retirementKey]: canonical(proof), ...(!qualified ? { [publicationKey]: canonical({ version: 1, attempt: "e".repeat(64), planDigest: "f".repeat(64), context: "kind-test", phase: "preparing" }) } : { "kars.azure.com/private-epoch": "current-epoch" }) } } };
    const owner = { annotations: { ...f.intentMap.metadata.annotations }, labels: { "app.kubernetes.io/managed-by": "Helm" } };
    const ca = "-----BEGIN CERTIFICATE-----\nrotated-public-ca\n-----END CERTIFICATE-----\n";
    f.catalog = { metadata: { ...identity("kars-inference-budget-contracts", "catalog-uid", "12"), namespace: ns.name,
      ...owner, annotations: { ...owner.annotations, "kars.azure.com/inference-budget-contracts": "v1" } }, data: { "ca.crt": ca } };
    f.budgetTls = { metadata: { ...identity("budget-tls", "tls-uid", "12"), namespace: ns.name, ...owner }, type: "kubernetes.io/tls" };
    f.root.Values.controller.replicas = qualified ? replicas : 0;
    f.root.Values.inferenceBudget.caBundle = ca;
    f.root.Values.evals.runnerImageBaseline = f.root.Values.evals.runnerImage;
    f.root.Values.evals.runnerImage = `localhost:5077/runner:latest@sha256:${"d".repeat(64)}`;
    return f;
  }
  function alterProof(f: any, change: (state: any, proof: any) => void) {
    const proof = JSON.parse(f.workspace.metadata.annotations[retirementKey]);
    const state = proof.version === 2 ? JSON.parse(proof.retirement) : proof;
    change(state, proof);
    if (proof.version === 2) { proof.retirement = canonical(state); proof.retirementDigest = hash(proof.retirement); }
    f.workspace.metadata.annotations[retirementKey] = canonical(proof);
  }
  function multiNamespaceFixture(): any {
    const f = privateFixture(true);
    const live = { apiVersion: "apps/v1", kind: "Deployment", metadata: { name: "sre", namespace: "kars-sre", uid: "sre-deployment-uid", resourceVersion: "14" },
      spec: { replicas: 1, template: { metadata: { annotations: { "kars.azure.com/private-epoch": "sre-epoch" } },
        spec: { serviceAccountName: "sre-api-router", containers: [{ name: "sre", image: "registry.example/sre:latest@sha256:" + "a".repeat(64) }] } } } };
    f.privateConsumers = { "kars-sre/Deployment/sre": live };
    alterProof(f, (state, proof) => {
      const scope = { namespace: { name: "kars-sre", uid: "sre-namespace-uid", resourceVersion: "13" }, epoch: "sre-epoch",
        consumers: [{ kind: "Deployment", object: { name: live.metadata.name, uid: live.metadata.uid, resourceVersion: live.metadata.resourceVersion }, templateDigest: templateDigest(live) }] };
      proof.activation.namespaces.unshift(scope);
      state.captured[scope.namespace.uid] = ["retired-sre-pod"];
      state.binding = retirementBinding(proof.activation);
      f.privateNamespaces = { "kars-sre": { metadata: { ...scope.namespace,
        annotations: { ...annotations(proof.activation, scope, "Qualified"), "kars.azure.com/private-epoch": scope.epoch } } } };
    });
    return f;
  }
  it("preserves qualified multi-namespace retirement with SRE before the root in receipt order", () => {
    const f = multiNamespaceFixture(), before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(f).toEqual(before);
  });
  it.each([
    ["missing namespace", (f: any) => { delete f.privateNamespaces["kars-sre"]; }],
    ["replaced namespace", (f: any) => { f.privateNamespaces["kars-sre"].metadata.uid = "replacement"; }],
    ["deleting namespace", (f: any) => { f.privateNamespaces["kars-sre"].metadata.deletionTimestamp = "2026-10-29T00:00:00Z"; }],
    ["namespace epoch", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-epoch"] = "wrong"; }],
    ["namespace authority", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-root-uid"] = "wrong"; }],
    ["namespace profile", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations[`kars.azure.com/private-${f.privateControllers[0]}-uid`] = "wrong"; }],
    ["missing SRE consumer", (f: any) => { delete f.privateConsumers["kars-sre/Deployment/sre"]; }],
    ["unscoped SRE consumer", (f: any) => { f.privateConsumers["Deployment/sre"] = f.privateConsumers["kars-sre/Deployment/sre"]; delete f.privateConsumers["kars-sre/Deployment/sre"]; }],
    ["consumer namespace", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].metadata.namespace = "kars-system"; }],
    ["consumer UID", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].metadata.uid = "replacement"; }],
    ["consumer epoch", (f: any) => { delete f.privateConsumers["kars-sre/Deployment/sre"].spec.template.metadata.annotations["kars.azure.com/private-epoch"]; }],
    ["consumer template", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].spec.template.spec.containers[0].image = "changed"; }],
    ["omitted namespace", (f: any) => alterProof(f, (_state, proof) => { proof.activation.namespaces.shift(); })],
    ["duplicate namespace", (f: any) => alterProof(f, (_state, proof) => { proof.activation.namespaces.push(structuredClone(proof.activation.namespaces[0])); })],
    ["unknown captured scope", (f: any) => alterProof(f, state => { state.captured["foreign-uid"] = []; })],
    ["changed binding", (f: any) => alterProof(f, state => { state.binding = "f".repeat(64); })],
  ] as const)("rejects multi-namespace continuity with %s", (_name, change) => {
    const f = multiNamespaceFixture(); change(f); expect(() => render(f)).toThrow();
  });
  function pendingMultiNamespaceFixture(): any {
    const f = multiNamespaceFixture();
    const fields = f.workspace.metadata.annotations;
    const proof = JSON.parse(fields[retirementKey]), state = JSON.parse(proof.retirement);
    state.phase = "retired";
    fields[retirementKey] = canonical(state);
    fields[publicationKey] = canonical({ version: 1, attempt: "e".repeat(64), planDigest: "f".repeat(64), context: "kind-test", phase: "preparing" });
    fields["kars.azure.com/private-state"] = "Pending";
    const scope = f.privateNamespaces["kars-sre"].metadata.annotations;
    scope["kars.azure.com/private-state"] = "Pending";
    scope["kars.azure.com/private-parent-sre-deployment-uid"] = "sre-epoch";
    f.controller.spec.replicas = f.root.Values.controller.replicas = f.privateConsumers["kars-sre/Deployment/sre"].spec.replicas = 0;
    f.registration.status.phase = "Provisioning";
    delete f.registration.status.privacyRevision;
    return f;
  }
  it("rejects Provisioning SRE outside an owned retired Pending publication", () => {
    const f = multiNamespaceFixture();
    f.registration.status.phase = "Provisioning";
    delete f.registration.status.privacyRevision;
    expect(() => render(f)).toThrow(/live original identities/);
  });
  it("preserves owned Pending multi-namespace publication with retired SRE consumers", () => {
    const f = pendingMultiNamespaceFixture(), before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]).controller.replicas).toBe(0);
    expect(f).toEqual(before);
  });
  it.each([
    ["unexpected registration phase", (f: any) => { f.registration.status.phase = "Failed"; }],
    ["unexpected privacy revision", (f: any) => { f.registration.status.privacyRevision = "foreign"; }],
    ["stale registration generation", (f: any) => { f.registration.status.observedGeneration = 0; }],
    ["disabled registration", (f: any) => { f.registration.spec.enabled = false; }],
    ["changed registration identity", (f: any) => { f.registration.spec.controller.deployment.uid = "replacement"; }],
    ["missing namespace", (f: any) => { delete f.privateNamespaces["kars-sre"]; }],
    ["replaced namespace", (f: any) => { f.privateNamespaces["kars-sre"].metadata.uid = "replacement"; }],
    ["deleting namespace", (f: any) => { f.privateNamespaces["kars-sre"].metadata.deletionTimestamp = "2026-11-04T00:00:00Z"; }],
    ["namespace name", (f: any) => { f.privateNamespaces["kars-sre"].metadata.name = "foreign"; }],
    ["namespace authority", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-root-uid"] = "wrong"; }],
    ["namespace state", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-state"] = "Qualified"; }],
    ["namespace profile", (f: any) => { f.privateNamespaces["kars-sre"].metadata.annotations[`kars.azure.com/private-${f.privateControllers[0]}-uid`] = "wrong"; }],
    ["parent enrollment", (f: any) => { delete f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-parent-sre-deployment-uid"]; }],
    ["missing consumer", (f: any) => { delete f.privateConsumers["kars-sre/Deployment/sre"]; }],
    ["unscoped consumer", (f: any) => { f.privateConsumers["Deployment/sre"] = f.privateConsumers["kars-sre/Deployment/sre"]; delete f.privateConsumers["kars-sre/Deployment/sre"]; }],
    ["running consumer", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].spec.replicas = 1; }],
    ["consumer namespace", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].metadata.namespace = "kars-system"; }],
    ["consumer UID", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].metadata.uid = "replacement"; }],
    ["consumer template", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].spec.template.spec.containers[0].image = "changed"; }],
    ["unsupported consumer", (f: any) => { f.privateConsumers["kars-sre/Deployment/sre"].kind = "StatefulSet"; }],
    ["unknown capture", (f: any) => alterProof(f, state => { state.captured["foreign-uid"] = []; })],
    ["changed binding", (f: any) => alterProof(f, state => { state.binding = "f".repeat(64); })],
    ["extra enrolled consumer", (f: any) => {
      const extra = structuredClone(f.privateConsumers["kars-sre/Deployment/sre"]);
      extra.metadata.name = "extra"; extra.metadata.uid = "extra-uid";
      f.privateConsumers["kars-sre/Deployment/extra"] = extra;
      f.privateNamespaces["kars-sre"].metadata.annotations["kars.azure.com/private-parent-extra-uid"] = "sre-epoch";
    }],
  ] as const)("rejects Pending multi-namespace publication with %s", (_name, change) => {
    const f = pendingMultiNamespaceFixture(); change(f); expect(() => render(f)).toThrow();
  });
  function pausingFixture(): any {
    const f = privateFixture();
    f.root.Values.inferenceBudget.caBundle = "original-public-ca";
    f.root.Values.controller.upgradePrecondition = { uid: f.controller.metadata.uid, resourceVersion: f.controller.metadata.resourceVersion };
    delete f.workspace.metadata.annotations[publicationKey];
    delete f.controller.spec.template.metadata.annotations["kars.azure.com/private-epoch"];
    alterProof(f, state => { state.phase = "pausing"; delete state.baseline; state.captured["sre-namespace-uid"] = ["sre-pod-uid"]; });
    return f;
  }
  it("publishes unchanged trust while preserving an initially paused root and multi-namespace retirement", () => {
    const f = pausingFixture(), before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.controller.replicas).toBe(0);
    expect(values.inferenceBudget.caBundle).toBe("original-public-ca");
    expect(f).toEqual(before);
  });
  it.each([
    ["missing fence", (f: any) => { delete f.root.Values.controller.upgradePrecondition; }],
    ["stale fence", (f: any) => { f.root.Values.controller.upgradePrecondition.resourceVersion = "9"; }],
    ["changed CA", (f: any) => { f.root.Values.inferenceBudget.caBundle = "changed"; }],
    ["changed root UID", (f: any) => { f.workspace.metadata.annotations["kars.azure.com/private-root-deployment-uid"] = "changed"; }],
    ["changed template", (f: any) => { f.controller.spec.template.spec.containers[0].image = "changed"; }],
    ["running root", (f: any) => { f.controller.spec.replicas = 1; }],
    ["missing adoption", (f: any) => { f.adoptionMap = {}; }],
    ["unsealed intent", (f: any) => alterProof(f, state => { state.replicaIntent = 2; })],
    ["unowned publication", (f: any) => { f.workspace.metadata.annotations[publicationKey] = "{}"; }],
    ["missing root capture", (f: any) => alterProof(f, state => { delete state.captured["workspace-uid"]; })],
    ["invalid exposed key", (f: any) => alterProof(f, state => { state.exposedKeys = ["invalid"]; })],
    ["other configuration", (f: any) => { f.root.Values.controller.extraEnv.push({ name: "NEW", value: "changed" }); }],
  ] as const)("rejects initial-pause maintenance with %s", (_name, change) => {
    const f = pausingFixture(); change(f); expect(() => render(f)).toThrow();
  });
  function initialPrivateFixture(): any {
    const f = privateFixture(true);
    f.adoptionMap = {};
    f.root.Values.evals.runnerImage = f.root.Values.evals.runnerImageBaseline;
    delete f.root.Values.evals.runnerImageBaseline;
    return f;
  }
  function extraPrivateConsumer(f: any, kind = "Deployment"): string {
    const name = "credential-writer", uid = "writer-uid";
    const apiVersion = ["Pod", "ReplicationController"].includes(kind) ? "v1" : ["Job", "CronJob"].includes(kind) ? "batch/v1" : "apps/v1";
    const template = structuredClone(f.controller.spec.template);
    template.spec.serviceAccountName = "credential-writer";
    const metadata = { name, namespace: "kars-system", uid, resourceVersion: "15" };
    const consumer = kind === "Pod" ? { apiVersion, kind, metadata: { ...template.metadata, ...metadata }, spec: template.spec }
      : { apiVersion, kind, metadata, spec: kind === "CronJob" ? { jobTemplate: { spec: { template } } } : { template } };
    const id = `${kind}/${name}`;
    f.privateConsumers = { [id]: consumer };
    alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].consumers.unshift({ kind, object: { name, uid, resourceVersion: "15" }, templateDigest: templateDigest(consumer) });
      state.binding = retirementBinding(proof.activation);
    });
    return id;
  }
  function pendingCompanionFixture(): { f: any; id: string } {
    const f = privateFixture(true), id = extraPrivateConsumer(f);
    const fields = f.workspace.metadata.annotations;
    const proof = JSON.parse(fields[retirementKey]), state = JSON.parse(proof.retirement);
    state.phase = "retired";
    fields[retirementKey] = canonical(state);
    fields[publicationKey] = canonical({ version: 1, attempt: "e".repeat(64), planDigest: "f".repeat(64), context: "kind-test", phase: "preparing" });
    fields["kars.azure.com/private-state"] = "Pending";
    fields["kars.azure.com/private-parent-writer-uid"] = "current-epoch";
    f.controller.spec.replicas = f.root.Values.controller.replicas = f.privateConsumers[id].spec.replicas = 0;
    return { f, id };
  }
  it("renders owned Pending publication with a bound, paused Deployment companion", () => {
    const { f } = pendingCompanionFixture();
    expect(render(f)).toContain("verified");
  });
  it.each([
    ["missing companion", (f: any, id: string) => { delete f.privateConsumers[id]; }],
    ["missing parent enrollment", (f: any) => { delete f.workspace.metadata.annotations["kars.azure.com/private-parent-writer-uid"]; }],
    ["replacement UID", (f: any, id: string) => { f.privateConsumers[id].metadata.uid = "replacement"; }],
    ["running companion", (f: any, id: string) => { f.privateConsumers[id].spec.replicas = 1; }],
    ["foreign namespace", (f: any, id: string) => { f.privateConsumers[id].metadata.namespace = "foreign"; }],
    ["deleting companion", (f: any, id: string) => { f.privateConsumers[id].metadata.deletionTimestamp = "2026-01-01T00:00:00Z"; }],
    ["changed template", (f: any, id: string) => { f.privateConsumers[id].spec.template.spec.containers[0].image = "changed:latest"; }],
    ["unsupported kind", (f: any, id: string) => { f.privateConsumers[id].kind = "StatefulSet"; }],
    ["changed API", (f: any, id: string) => { f.privateConsumers[id].apiVersion = "apps/v2"; }],
    ["extra enrolled companion", (f: any, id: string) => {
      const extra = structuredClone(f.privateConsumers[id]);
      extra.metadata.name = "extra"; extra.metadata.uid = "extra-uid";
      f.privateConsumers["Deployment/extra"] = extra;
      f.workspace.metadata.annotations["kars.azure.com/private-parent-extra-uid"] = "current-epoch";
    }],
  ] as const)("rejects Pending publication with %s", (_name, change) => {
    const { f, id } = pendingCompanionFixture(); change(f, id);
    expect(() => render(f)).toThrow();
  });
  it("does not add unrelated, unenrolled Deployments to Pending retirement scope", () => {
    const { f, id } = pendingCompanionFixture();
    const unrelated = structuredClone(f.privateConsumers[id]);
    unrelated.metadata.name = "unrelated"; unrelated.metadata.uid = "unrelated-uid"; unrelated.spec.replicas = 1;
    f.privateConsumers["Deployment/unrelated"] = unrelated;
    expect(render(f)).toContain("verified");
  });
  const privateDefinition = JSON.parse(readFileSync(`${root}deploy/helm/kars/files/private-consumption.json`, "utf8"));
  const secret = privateDefinition.secrets[0];
  const classificationCases: [string, any, boolean][] = [
    ["ordinary credential writer", {}, false],
    ["ordinary API token", { volumes: [{ projected: { sources: [{ serviceAccountToken: { audience: "api" } }] } }] }, false],
    ["unrelated Secret", { volumes: [{ secret: { secretName: "ordinary-credential" } }] }, false],
    ["root API token", { serviceAccountName: "kars-controller" }, true],
    ["root without token", { serviceAccountName: "kars-controller", automountServiceAccountToken: false }, false],
    ["root projected token", { serviceAccountName: "kars-controller", automountServiceAccountToken: false,
      volumes: [{ projected: { sources: [{ serviceAccountToken: {} }] } }] }, true],
    ["budget TLS", { volumes: [{ secret: { secretName: "budget-tls" } }] }, true],
    ["private Secret", { volumes: [{ secret: { secretName: secret } }] }, true],
    ["private CSI Secret", { volumes: [{ csi: { nodePublishSecretRef: { name: secret } } }] }, true],
    ["private projected Secret", { volumes: [{ projected: { sources: [{ secret: { name: secret } }] } }] }, true],
    ["private pull Secret", { imagePullSecrets: [{ name: secret }] }, true],
    ["host path", { volumes: [{ hostPath: {} }] }, true],
  ];
  for (const audience of privateDefinition.tokenAudiences) classificationCases.push([
    `private token ${audience}`, { volumes: [{ projected: { sources: [{ serviceAccountToken: { audience } }] } }] }, true,
  ]);
  for (const field of ["hostNetwork", "hostPID", "hostIPC"]) classificationCases.push([field, { [field]: true }, true]);
  for (const kind of ["azureFile", "cephfs", "cinder", "flexVolume", "iscsi", "rbd", "scaleIO", "storageos"]) {
    classificationCases.push([`${kind} name`, { volumes: [{ [kind]: { secretName: secret } }] }, true]);
    classificationCases.push([`${kind} ref`, { volumes: [{ [kind]: { secretRef: { name: secret } } }] }, true]);
  }
  for (const group of ["containers", "initContainers", "ephemeralContainers"]) {
    for (const [name, fields] of [
      ["envFrom", { envFrom: [{ secretRef: { name: secret } }] }],
      ["env", { env: [{ name: "KEY", valueFrom: { secretKeyRef: { name: secret, key: "key" } } }] }],
      ["privileged", { securityContext: { privileged: true } }],
    ] as const) classificationCases.push([`${group} ${name}`, { [group]: [{ name: "consumer", ...fields }] }, true]);
    for (const capability of ["ALL", "SYS_ADMIN", "SYS_PTRACE", "SYS_MODULE", "SYS_RAWIO", "BPF", "PERFMON", "CHECKPOINT_RESTORE", "DAC_READ_SEARCH"]) {
      classificationCases.push([`${group} ${capability}`, { [group]: [{ name: "consumer", securityContext: { capabilities: { add: [capability] } } }] }, true]);
    }
    classificationCases.push([`${group} ordinary capability`, { [group]: [{ name: "consumer", securityContext: { capabilities: { add: ["NET_BIND_SERVICE"] } } }] }, false]);
  }
  it.each(classificationCases)("matches existing private-authority classification for %s", (_name, fields, expected) => {
    const f = initialPrivateFixture(), id = extraPrivateConsumer(f);
    const live = f.privateConsumers[id];
    Object.assign(live.spec.template.spec, fields);
    delete live.spec.template.metadata.annotations;
    alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].consumers[0].templateDigest = templateDigest(live);
      state.binding = retirementBinding(proof.activation);
    });
    const proof = JSON.parse(f.workspace.metadata.annotations[retirementKey]);
    expect(consumesPrivateAuthority(live, "kars-system", proof.activation)).toBe(expected);
    if (expected) expect(() => render(f)).toThrow(/live consumer epoch changed/);
    else expect(render(f)).toContain("verified");
    live.spec.template.metadata.annotations = { "kars.azure.com/private-epoch": "current-epoch" };
    expect(render(f)).toContain("verified");
  });
  it.each(["Ready", "Blocked"])("adopts the sealed initial intent after independently qualified CA rotation with SRE %s", phase => {
    const f = initialPrivateFixture();
    extraPrivateConsumer(f);
    f.registration.status.phase = phase;
    if (phase === "Blocked") delete f.registration.status.privacyRevision;
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON());
    const selected = JSON.parse(docs.find(d => d?.metadata?.name === "selected-values").data["values.json"]);
    expect(selected.inferenceBudget.caBundle).toBe(f.catalog.data["ca.crt"]);
    expect(selected.controller.replicas).toBe(1);
    const original = structuredClone(f.root.Values);
    delete original.controller.image;
    original.inferenceBudget.caBundle = JSON.parse(f.intentMap.data["intent.json"]).budget.caBundle;
    expect(docs.find(d => d?.metadata?.name === "verified").data.valuesDigest).toBe(hash(canonicalSchema(original)));
  });
  it.each([
    ["unrelated image change", (f: any) => { f.root.Values.controller.image.digest = `sha256:${"f".repeat(64)}`; }],
    ["unrelated runtime change", (f: any) => { f.root.Values.evals.runnerImage += "changed"; }],
    ["changed funding", (f: any) => { f.root.Values.sre.firstInstallFunding.tokens++; }],
    ["stale SRE generation", (f: any) => { f.registration.status.observedGeneration = 0; }],
    ["unqualified migration", (f: any) => { f.registration.status.phase = "Migrating"; }],
    ["disabled authority", (f: any) => { f.registration.spec.enabled = false; }],
    ["changed source UID", (f: any) => { f.source.metadata.uid = "other"; }],
    ["changed root UID", (f: any) => { f.controller.metadata.uid = "other"; }],
    ["changed runtime UID", (f: any) => { f.runtime.metadata.uid = "other"; }],
    ["paused root", (f: any) => { f.controller.spec.replicas = 0; f.root.Values.controller.replicas = 0; }],
    ["missing retirement", (f: any) => { delete f.workspace.metadata.annotations[retirementKey]; }],
    ["changed retired binding", (f: any) => alterProof(f, state => { state.binding = "0".repeat(64); })],
  ] as const)("rejects initial private adoption with %s", (_name, change) => {
    const f = initialPrivateFixture();
    f.registration.status.phase = "Blocked";
    delete f.registration.status.privacyRevision;
    change(f);
    expect(() => render(f)).toThrow();
  });
  it.each(["Deployment", "ReplicaSet", "StatefulSet", "DaemonSet", "ReplicationController", "Job", "CronJob", "Pod"])(
    "verifies the actual additional %s consumer and order-independent retirement binding", kind => {
      const f = privateFixture(true);
      extraPrivateConsumer(f, kind);
      expect(render(f)).toContain("verified");
      alterProof(f, (_state, proof) => { proof.activation.namespaces[0].consumers.reverse(); });
      expect(render(f)).toContain("verified");
    });
  it.each([
    ["missing live object", (f: any, id: string) => { delete f.privateConsumers[id]; }],
    ["changed UID", (f: any, id: string) => { f.privateConsumers[id].metadata.uid = "replacement"; }],
    ["changed namespace", (f: any, id: string) => { f.privateConsumers[id].metadata.namespace = "other"; }],
    ["deleting object", (f: any, id: string) => { f.privateConsumers[id].metadata.deletionTimestamp = "now"; }],
    ["changed kind", (f: any, id: string) => { f.privateConsumers[id].kind = "Secret"; }],
    ["changed API", (f: any, id: string) => { f.privateConsumers[id].apiVersion = "v1"; }],
    ["stale epoch", (f: any, id: string) => { f.privateConsumers[id].spec.template.metadata.annotations["kars.azure.com/private-epoch"] = "old"; }],
    ["changed template", (f: any, id: string) => { f.privateConsumers[id].spec.template.spec.serviceAccountName = "other"; }],
    ["self-consistent changed template", (f: any, id: string) => {
      f.privateConsumers[id].spec.template.spec.serviceAccountName = "other";
      alterProof(f, (_state, proof) => { proof.activation.namespaces[0].consumers[0].templateDigest = templateDigest(f.privateConsumers[id]); });
    }],
    ["duplicate consumer", (f: any) => alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].consumers.push(proof.activation.namespaces[0].consumers[0]); state.binding = retirementBinding(proof.activation);
    })],
    ["missing root consumer", (f: any) => alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].consumers.pop(); state.binding = retirementBinding(proof.activation);
    })],
    ["foreign scope", (f: any) => alterProof(f, (state, proof) => {
      proof.activation.namespaces[0].namespace.name = "other"; state.binding = retirementBinding(proof.activation);
    })],
  ] as const)("rejects additional qualified consumer with %s", (_name, change) => {
    const f = privateFixture(true), id = extraPrivateConsumer(f);
    change(f, id);
    expect(() => render(f)).toThrow();
  });
  it.each([false, true])("retains the public CA and exact adoption receipt with qualified=%s", qualified => {
    const f = privateFixture(qualified, 3), original = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON());
    const selected = JSON.parse(docs.find(d => d?.metadata?.name === "selected-values").data["values.json"]);
    expect(selected.controller.replicas).toBe(qualified ? 3 : 0);
    expect(selected.inferenceBudget.caBundle).toBe(f.catalog.data["ca.crt"]);
    expect(selected.evals).toEqual(f.root.Values.evals);
    expect(f).toEqual(original);
    expect(docs.find(d => d?.metadata?.name === "verified").data.valuesDigest).toBe(f.adoptionMap.data.valuesDigest);
  });
  it.each([false, true])("retains externally managed TLS without adopting it with qualified=%s", qualified => {
    for (const manager of [undefined, "external-issuer"]) {
      const f = privateFixture(qualified);
      delete f.budgetTls.metadata.annotations;
      f.budgetTls.metadata.labels = manager ? { "app.kubernetes.io/managed-by": manager } : {};
      f.budgetTls.data = { "tls.crt": Buffer.from("rotated-certificate").toString("base64"), "tls.key": Buffer.from("rotated-key").toString("base64") };
      const original = structuredClone(f);
      const output = render(f, true);
      const docs = parseAllDocuments(output).map(d => d.toJSON());
      const selected = JSON.parse(docs.find(d => d?.metadata?.name === "selected-values").data["values.json"]);
      expect(selected.inferenceBudget.tls).toEqual(original.root.Values.inferenceBudget.tls);
      expect(docs.find(d => d?.metadata?.name === "verified").data.valuesDigest).toBe(f.adoptionMap.data.valuesDigest);
      expect(output).not.toContain("rotated-key");
      expect(output).not.toContain(f.budgetTls.data["tls.key"]);
      expect(f).toEqual(original);
    }
  });
  it.each([false, true])("requires release ownership for chart-managed TLS with qualified=%s", qualified => {
    const f = privateFixture(qualified, 1, true);
    expect(render(f)).toContain("verified");
    delete f.budgetTls.metadata.annotations;
    delete f.budgetTls.metadata.labels;
    expect(() => render(f)).toThrow(/release-owned catalog and TLS identities/);
  });
  it.each([
    ["replaced UID", (f: any) => { f.budgetTls.metadata.uid = "replacement"; }],
    ["changed version", (f: any) => { f.budgetTls.metadata.resourceVersion = "13"; }],
    ["deleting Secret", (f: any) => { f.budgetTls.metadata.deletionTimestamp = "now"; }],
    ["wrong namespace", (f: any) => { f.budgetTls.metadata.namespace = "other"; }],
    ["different selected Secret", (f: any) => { f.root.Values.inferenceBudget.tlsSecretName = "other"; }],
    ["missing Secret", (f: any) => { f.budgetTls = {}; }],
  ] as const)("rejects external TLS continuity with %s", (_name, change) => {
    const f = privateFixture(true);
    delete f.budgetTls.metadata.annotations;
    delete f.budgetTls.metadata.labels;
    expect(render(f)).toContain("verified");
    change(f);
    expect(() => render(f)).toThrow();
  });
  it("accepts the maintained certificate controller profile", () => {
    const f = privateFixture(true);
    alterProof(f, (state, proof) => {
      proof.activation.profile = "kcm-certificate"; proof.activation.controllerUids = {};
      state.binding = retirementBinding(proof.activation);
      f.workspace.metadata.annotations["kars.azure.com/private-profile"] = "kcm-certificate";
    });
    expect(render(f)).toContain("verified");
  });
  it.each([
    ["no completed adoption", (f: any) => { f.adoptionMap = {}; }],
    ["missing publication", (f: any) => { delete f.workspace.metadata.annotations[publicationKey]; }],
    ["publication already launched", (f: any) => { const p = JSON.parse(f.workspace.metadata.annotations[publicationKey]); p.phase = "publishing"; f.workspace.metadata.annotations[publicationKey] = canonical(p); }],
    ["changed publication shape", (f: any) => { const p = JSON.parse(f.workspace.metadata.annotations[publicationKey]); p.extra = true; f.workspace.metadata.annotations[publicationKey] = canonical(p); }],
    ["not retired", (f: any) => alterProof(f, s => { s.phase = "pausing"; })],
    ["unbound retirement", (f: any) => alterProof(f, s => { s.binding = "0".repeat(64); })],
    ["unknown receipt field", (f: any) => alterProof(f, s => { s.extra = true; })],
    ["false pauseRoot", (f: any) => alterProof(f, s => { s.pauseRoot = false; })],
    ["changed replica intent", (f: any) => alterProof(f, s => { s.replicaIntent = 2; })],
    ["self-consistent but unsealed replica intent", (f: any) => alterProof(f, s => { const other = JSON.parse(privateFixture(false, 2).workspace.metadata.annotations[retirementKey]); s.replicaIntent = other.replicaIntent; s.binding = other.binding; })],
    ["mutable runner image", (f: any) => { f.root.Values.evals.runnerImage = "localhost:5077/runner:latest"; }],
    ["forged runner baseline", (f: any) => { f.root.Values.evals.runnerImageBaseline = "localhost:5077/other:latest"; }],
    ["running root", (f: any) => { f.controller.spec.replicas = 1; }],
    ["changed root UID", (f: any) => { f.controller.metadata.uid = "replacement"; }],
    ["changed namespace UID", (f: any) => { f.workspace.metadata.uid = "replacement"; }],
    ["changed service account", (f: any) => { f.controller.spec.template.spec.serviceAccountName = "other"; }],
    ["changed template", (f: any) => { f.controller.spec.template.spec.containers[0].image += "changed"; }],
    ["missing controller profile identity", (f: any) => { delete f.workspace.metadata.annotations["kars.azure.com/private-job-controller-uid"]; }],
    ["changed TLS UID", (f: any) => { f.budgetTls.metadata.uid = "replacement"; }],
    ["unchanged TLS version", (f: any) => { f.budgetTls.metadata.resourceVersion = "11"; }],
    ["missing retired TLS baseline", (f: any) => alterProof(f, s => { delete s.baseline; })],
    ["missing exposed-key evidence", (f: any) => alterProof(f, s => { s.exposedKeys = []; })],
    ["foreign captured scope", (f: any) => alterProof(f, s => { s.captured.other = []; })],
    ["wrong TLS type", (f: any) => { f.budgetTls.type = "Opaque"; }],
    ["foreign catalog", (f: any) => { f.catalog.metadata.annotations["meta.helm.sh/release-name"] = "other"; }],
    ["deleting catalog", (f: any) => { f.catalog.metadata.deletionTimestamp = "now"; }],
    ["non-live CA", (f: any) => { f.root.Values.inferenceBudget.caBundle += "different"; }],
    ["private CA material", (f: any) => { f.root.Values.inferenceBudget.caBundle += "PRIVATE KEY"; f.catalog.data["ca.crt"] = f.root.Values.inferenceBudget.caBundle; }],
    ["funding changed", (f: any) => { f.root.Values.sre.firstInstallFunding.tokens++; }],
    ["router verifier changed", (f: any) => { f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`; }],
    ["adoption digest changed", (f: any) => { f.adoptionMap.data.valuesDigest = "0".repeat(64); }],
  ] as const)("rejects paused publication with %s", (_name, change) => { const f = privateFixture(); change(f); expect(() => render(f)).toThrow(); });
  it.each([
    ["stale published replica zero", (f: any) => { f.root.Values.controller.replicas = 0; }],
    ["wrong restored replicas", (f: any) => { f.controller.spec.replicas = 2; }],
    ["bad retirement digest", (f: any) => { const p = JSON.parse(f.workspace.metadata.annotations[retirementKey]); p.retirementDigest = "0".repeat(64); f.workspace.metadata.annotations[retirementKey] = canonical(p); }],
    ["wrong activation phase", (f: any) => alterProof(f, (_s, p) => { p.activation.phase = "reviewed"; })],
    ["changed epoch", (f: any) => { f.controller.spec.template.metadata.annotations["kars.azure.com/private-epoch"] = "other"; }],
    ["different activation account", (f: any) => alterProof(f, (_s, p) => { p.activation.root.account.uid = "replacement"; })],
    ["additional consumer", (f: any) => alterProof(f, (_s, p) => { p.activation.namespaces[0].consumers.push(p.activation.namespaces[0].consumers[0]); })],
    ["different consumer template", (f: any) => alterProof(f, (_s, p) => { p.activation.namespaces[0].consumers[0].templateDigest = "0".repeat(64); })],
    ["stale qualified TLS", (f: any) => { f.budgetTls.metadata.resourceVersion = "13"; }],
    ["previously exposed key", (f: any) => alterProof(f, s => { s.exposedKeys.push("c".repeat(64)); })],
  ] as const)("rejects qualified continuity with %s", (_name, change) => { const f = privateFixture(true); change(f); expect(() => render(f)).toThrow(); });
});
