// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { copyFileSync, cpSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";
import { root, adoptedFixture, hash, render, fixture } from "./sre-first-install-transition.test-support.js";

describe("adopted optional Grafana lifecycle", () => {
  function coalesceMonitoring(monitoring: any) {
    const dir = `${root}.sre-grafana-check-${randomUUID()}`;
    mkdirSync(`${dir}/templates`, { recursive: true });
    try {
      for (const file of ["Chart.yaml", "values.yaml"]) copyFileSync(`${root}deploy/helm/kars/${file}`, `${dir}/${file}`);
      cpSync(`${root}deploy/helm/kars/charts`, `${dir}/charts`, { recursive: true });
      writeFileSync(`${dir}/selected.json`, JSON.stringify({ monitoring }));
      writeFileSync(`${dir}/templates/values.yaml`, 'apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: computed-monitoring\ndata:\n  values.json: {{ .Values.monitoring | toJson | quote }}\n');
      const output = execFileSync("helm", ["template", "kars", dir, "-n", "kars-system", "-f", `${dir}/selected.json`],
        { encoding: "utf8", timeout: 10000, maxBuffer: 16 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] });
      const doc = parseAllDocuments(output).map(d => d.toJSON()).find(d => d?.metadata?.name === "computed-monitoring");
      return JSON.parse(doc.data["values.json"]);
    } finally { rmSync(dir, { recursive: true, force: true }); }
  }
  function grafanaFixture(enabled = false, monitoring?: any) {
    const f = adoptedFixture();
    f.root.Values.monitoring = { enabled: true, prometheus: { enabled: true }, grafana: {
      enabled, image: { registry: "registry.example:5443", repository: "qualified/grafana", tag: "latest", pullPolicy: "Always" },
      sidecar: { image: { registry: "registry.example:5443", repository: "qualified/sidecar", tag: "latest" } },
      "grafana.ini": { "auth.anonymous": { enabled: false } },
      resources: { limits: { memory: "512Mi" } }, serviceAccount: { create: true }, rbac: { namespaced: true },
    } };
    if (monitoring) f.root.Values.monitoring = monitoring;
    const original = structuredClone(f.root.Values);
    delete original.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(original));
    const grafana = f.root.Values.monitoring.grafana;
    f.root.Values.monitoring.grafanaBaseline = structuredClone(grafana);
    grafana.enabled = !enabled;
    for (const image of [grafana.image, grafana.sidecar.image]) {
      image.registry = "another.registry.example:5443";
      image.tag = `latest@sha256:${"b".repeat(64)}`;
    }
    return f;
  }
  it.each([false, true])("enables, refreshes, disables and re-enables Grafana against its original enabled=%s receipt", enabled => {
    const f = grafanaFixture(enabled);
    const receipt = structuredClone(f.adoptionMap.data);
    for (const selected of [true, false, true]) {
      f.root.Values.monitoring.grafana.enabled = selected;
      f.root.Values.monitoring.grafana.image.tag = `latest@sha256:${(selected ? "c" : "d").repeat(64)}`;
      const before = structuredClone(f);
      const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
      expect(docs.find(d => d.metadata.name === "verified").data).toEqual(receipt);
      expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
      expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
      expect(f).toEqual(before);
    }
  });
  it.each([false, true])("preserves the receipt across actual dependency coalescing from enabled=%s", enabled => {
    const f = grafanaFixture(enabled, coalesceMonitoring({ grafana: { enabled } }));
    for (const selected of [true, false, true]) {
      f.root.Values.monitoring.grafana.enabled = selected;
      f.root.Values.monitoring = coalesceMonitoring(f.root.Values.monitoring);
      const docs = parseAllDocuments(render(f)).map(d => d.toJSON()).filter(Boolean);
      expect(docs.find(d => d.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    }
    f.root.Values.monitoring.grafana["grafana.ini"]["auth.anonymous"] = { enabled: true };
    expect(() => render(f)).toThrow("preserve authentication");
  });
  it("tightens a legacy cluster-wide Grafana baseline without permitting re-expansion", () => {
    const f = grafanaFixture(false, coalesceMonitoring({ grafana: { enabled: false, rbac: { namespaced: false } } }));
    f.root.Values.monitoring.grafana.rbac.namespaced = true;
    expect(() => render(f)).not.toThrow();
    f.root.Values.monitoring.grafana.rbac.namespaced = false;
    expect(() => render(f)).toThrow("namespace-scoped RBAC");
    f.root.Values.monitoring.grafana.rbac.namespaced = true;
    f.root.Values.monitoring.grafana.rbac.namespaces = ["another-tenant"];
    expect(() => render(f)).toThrow("preserve authentication");
  });
  it("leaves existing sealed configurations valid without adding a default baseline", () => {
    const f = grafanaFixture();
    const original = f.root.Values.monitoring.grafanaBaseline;
    f.root.Values.monitoring.grafana.enabled = original.enabled;
    Object.assign(f.root.Values.monitoring.grafana.image, original.image);
    Object.assign(f.root.Values.monitoring.grafana.sidecar.image, original.sidecar.image);
    delete f.root.Values.monitoring.grafanaBaseline;
    expect(() => render(f)).not.toThrow();
  });
  it.each(["adoption", "missing-baseline", "forged-baseline", "last-selected-baseline", "baseline-type", "baseline-extra", "switch-type",
    "missing-image-field", "image-extra", "image-type", "mutable-grafana", "mutable-sidecar", "bad-digest", "auth", "rbac", "resources",
    "sidecar-rbac", "prometheus", "funding", "source", "controller", "receipt", "hook"])("rejects changed %s", fault => {
    const f = grafanaFixture();
    const baseline = f.root.Values.monitoring.grafanaBaseline;
    const grafana = f.root.Values.monitoring.grafana;
    if (fault === "adoption") f.adoptionMap = {};
    if (fault === "missing-baseline") delete f.root.Values.monitoring.grafanaBaseline;
    if (fault === "forged-baseline") baseline.enabled = true;
    if (fault === "last-selected-baseline") baseline.image.tag = grafana.image.tag;
    if (fault === "baseline-type") f.root.Values.monitoring.grafanaBaseline = [];
    if (fault === "baseline-extra") baseline.authentication = false;
    if (fault === "switch-type") grafana.enabled = "true";
    if (fault === "missing-image-field") { delete baseline.image.registry; baseline.image.extra = "ignored"; }
    if (fault === "image-extra") baseline.sidecar.image.extra = "ignored";
    if (fault === "image-type") grafana.image.repository = {};
    if (fault === "mutable-grafana") grafana.image.tag = "latest";
    if (fault === "mutable-sidecar") grafana.sidecar.image.tag = "latest";
    if (fault === "bad-digest") grafana.sidecar.image.tag += "bad";
    if (fault === "auth") grafana["grafana.ini"]["auth.anonymous"].enabled = true;
    if (fault === "rbac") grafana.serviceAccount.create = false;
    if (fault === "resources") grafana.resources.limits.memory = "8Gi";
    if (fault === "sidecar-rbac") grafana.sidecar.enableUniqueFilenames = true;
    if (fault === "prometheus") f.root.Values.monitoring.prometheus.enabled = false;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.metadata.uid = "foreign";
    if (fault === "controller") f.controller.metadata.uid = "foreign";
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "forged";
    if (fault === "hook") f.root.Values.schemaHook.enabled = true;
    expect(() => render(f)).toThrow();
  });
});

describe("adopted Headlamp telemetry transitions", () => {
  const readers = [{ kind: "User", name: "mapped:operator@example.test" }, { kind: "Group", name: "mapped:auditors" }];
  const originalImage = "registry.example/headlamp";
  function telemetryFixture(originalSubjects: unknown[] = [], originalTag = "latest", adopted = true): any {
    const f = fixture();
    f.root.Values.headlamp = { enabled: true,
      image: { repository: originalImage, tag: originalTag, pullPolicy: "Always" }, imagePullSecrets: [],
      oidc: { enabled: true, issuerURL: "https://identity.example", clientID: "headlamp",
        clientSecret: { name: "existing-oidc", key: "client-secret" } } };
    f.root.Values.monitoring = { enabled: true, namespaceOverride: "monitoring", fullnameOverride: "kps-kube-prometheus-stack",
      prometheus: { enabled: true }, querySubjects: structuredClone(originalSubjects) };
    const original = structuredClone(f.root.Values);
    original.schemaHook.enabled = true;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.valuesDigest = hash(canonicalSchema(original));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    if (adopted) {
      const baseline = structuredClone(f.root.Values);
      delete baseline.controller.image;
      f.adoptionMap = structuredClone(adoptedFixture().adoptionMap);
      f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    }
    f.root.Values.headlamp.imageBaseline = `${originalImage}:${originalTag}`;
    f.root.Values.headlamp.image.repository = "another.registry.example:5443/qualified/headlamp";
    f.root.Values.headlamp.image.tag = `latest@sha256:${"b".repeat(64)}`;
    f.root.Values.monitoring.querySubjectsBaseline = structuredClone(originalSubjects);
    f.root.Values.monitoring.querySubjects = structuredClone(readers);
    return f;
  }
  function coldTelemetryFixture(): any {
    const f = telemetryFixture([], "latest", false);
    f.root.Values.evals = { runnerImageBaseline: "registry.example/runner:latest",
      runnerImage: `registry.example/runner:latest@sha256:${"b".repeat(64)}` };
    const baseline = { enabled: false, image: { registry: "registry.example", repository: "grafana", tag: "latest" },
      sidecar: { image: { registry: "registry.example", repository: "sidecar", tag: "latest" } }, rbac: { namespaced: true } };
    f.root.Values.monitoring.grafanaBaseline = baseline;
    f.root.Values.monitoring.grafana = structuredClone(baseline);
    f.root.Values.monitoring.grafana.image.tag = `latest@sha256:${"c".repeat(64)}`;
    f.root.Values.monitoring.grafana.sidecar.image.tag = `latest@sha256:${"d".repeat(64)}`;
    const original = structuredClone(f.root.Values);
    original.schemaHook.enabled = true;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.valuesDigest = hash(canonicalSchema(original));
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    return f;
  }
  it("adopts originally sealed runner, Headlamp and monitoring baselines before a controller update", () => {
    const f = coldTelemetryFixture();
    const originalValues = structuredClone(f.root.Values);
    const receipt = parseAllDocuments(render(f))[0].toJSON().data;
    expect(f.root.Values).toEqual(originalValues);
    f.adoptionMap = { ...adoptedFixture().adoptionMap, data: receipt };
    f.root.Values.controller.image.tag = `latest@sha256:${"e".repeat(64)}`;
    f.root.Values.controller.image.digest = `sha256:${"e".repeat(64)}`;
    expect(() => render(f)).not.toThrow();
  });
  it.each(["headlamp", "querySubjects", "grafana", "controller", "runner"])(
    "rejects %s changes before adopting the exact cold installation", field => {
      const f = coldTelemetryFixture();
      if (field === "headlamp") f.root.Values.headlamp.image.tag = `latest@sha256:${"e".repeat(64)}`;
      if (field === "querySubjects") f.root.Values.monitoring.querySubjects = [];
      if (field === "grafana") f.root.Values.monitoring.grafana.enabled = true;
      if (field === "controller") f.root.Values.controller.image.digest = `sha256:${"e".repeat(64)}`;
      if (field === "runner") f.root.Values.evals.runnerImage = `registry.example/runner:latest@sha256:${"e".repeat(64)}`;
      expect(() => render(f)).toThrow();
    });
  it.each([false, true])("preserves unchanged sealed values without new baseline defaults (adopted: %s)", adopted => {
    const f = telemetryFixture([], "latest", adopted);
    delete f.root.Values.headlamp.imageBaseline;
    Object.assign(f.root.Values.headlamp.image, { repository: originalImage, tag: "latest" });
    delete f.root.Values.monitoring.querySubjectsBaseline;
    f.root.Values.monitoring.querySubjects = [];
    expect(() => render(f)).not.toThrow();
  });
  it.each(["latest", `latest@sha256:${"a".repeat(64)}`])(
    "refreshes original %s images while adding, changing and revoking initially empty or nonempty readers", originalTag => {
      for (const originalSubjects of [[], [{ kind: "Group", name: "original:readers" }]]) {
        const f = telemetryFixture(originalSubjects, originalTag);
        const receipt = structuredClone(f.adoptionMap);
        for (const selectedSubjects of [readers, [{ kind: "Group", name: "replacement:readers" }], []]) {
          f.root.Values.monitoring.querySubjects = structuredClone(selectedSubjects);
          const before = structuredClone(f);
          const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
          expect(docs.find(d => d.metadata.name === "verified").data).toEqual(receipt.data);
          expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
          expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
          expect(f).toEqual(before);
        }
      }
    });
  it.each(["image", "subjects"])("supports the independent %s transition", transition => {
    const f = telemetryFixture();
    if (transition === "image") {
      delete f.root.Values.monitoring.querySubjectsBaseline;
      f.root.Values.monitoring.querySubjects = [];
    } else {
      delete f.root.Values.headlamp.imageBaseline;
      Object.assign(f.root.Values.headlamp.image, { repository: originalImage, tag: "latest" });
    }
    expect(() => render(f)).not.toThrow();
    f.adoptionMap = {};
    expect(() => render(f)).toThrow("completed SRE adoption");
  });
  it("retains the original baselines across image upgrades and composes with router and controller refreshes", () => {
    const f = telemetryFixture();
    const receipt = structuredClone(f.adoptionMap.data);
    f.root.Values.sre.routerImageBaseline = `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}`;
    f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"c".repeat(64)}`;
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"c".repeat(64)}`;
    f.root.Values.controller.image.tag = `latest@sha256:${"d".repeat(64)}`;
    for (const digest of ["b", "e"]) {
      f.root.Values.headlamp.image.tag = `latest@sha256:${digest.repeat(64)}`;
      const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
      expect(docs.find(d => d.metadata.name === "verified").data).toEqual(receipt);
      expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(f.root.Values);
    }
  });
  it.each(["image-missing", "image-forged", "image-last-selected", "subjects-missing", "subjects-forged", "subjects-last-selected"])(
    "rejects unbound or replaced originals: %s", fault => {
      const f = telemetryFixture([{ kind: "Group", name: "original:readers" }]);
      if (fault === "image-missing") delete f.root.Values.headlamp.imageBaseline;
      if (fault === "image-forged") f.root.Values.headlamp.imageBaseline = "foreign.example/headlamp:latest";
      if (fault === "image-last-selected") f.root.Values.headlamp.imageBaseline = `${f.root.Values.headlamp.image.repository}:${f.root.Values.headlamp.image.tag}`;
      if (fault === "subjects-missing") delete f.root.Values.monitoring.querySubjectsBaseline;
      if (fault === "subjects-forged") f.root.Values.monitoring.querySubjectsBaseline = [];
      if (fault === "subjects-last-selected") f.root.Values.monitoring.querySubjectsBaseline = structuredClone(readers);
      expect(() => render(f)).toThrow("original adoption receipt and unchanged values");
    });
  it.each(["baseline-type", "baseline-empty", "baseline-tag", "baseline-digest", "image-type", "repository-type", "tag-type", "mutable-selected", "selected-digest", "selected-url", "selected-tag-prefix"])(
    "rejects invalid image references: %s", fault => {
      const f = telemetryFixture();
      if (fault === "baseline-type") f.root.Values.headlamp.imageBaseline = {};
      if (fault === "baseline-empty") f.root.Values.headlamp.imageBaseline = "";
      if (fault === "baseline-tag") f.root.Values.headlamp.imageBaseline = `${originalImage}:v1`;
      if (fault === "baseline-digest") f.root.Values.headlamp.imageBaseline = `${originalImage}:latest@sha256:bad`;
      if (fault === "image-type") f.root.Values.headlamp.image = "invalid";
      if (fault === "repository-type") f.root.Values.headlamp.image.repository = {};
      if (fault === "tag-type") f.root.Values.headlamp.image.tag = {};
      if (fault === "mutable-selected") f.root.Values.headlamp.image.tag = "latest";
      if (fault === "selected-digest") f.root.Values.headlamp.image.tag += "bad";
      if (fault === "selected-tag-prefix") f.root.Values.headlamp.image.tag = `wrong:${f.root.Values.headlamp.image.tag}`;
      if (fault === "selected-url") f.root.Values.headlamp.image.repository = "https://registry.example/headlamp?unreviewed=true";
      expect(() => render(f)).toThrow("Headlamp image");
    });
  const invalidSubjects = [null, {}, "readers", ["reader"], [{}], [{ kind: "Group" }], [{ name: "reader" }],
    [{ kind: "ServiceAccount", name: "headlamp" }], [{ kind: "User", name: "reader", namespace: "headlamp" }],
    [{ kind: "User", name: "reader", apiGroup: "rbac.authorization.k8s.io" }], [{ kind: "Group", name: "" }],
    [{ kind: "Group", name: " reader" }], [{ kind: "Group", name: "reader " }], [{ kind: "User", name: "reader\nother" }],
    [{ kind: "Group", name: 3 }], [{ kind: false, name: "reader" }], [readers[0], readers[0]]];
  it.each(["querySubjectsBaseline", "querySubjects"])("validates every original and selected subject in %s", field => {
    for (const subjects of invalidSubjects) {
      const f = telemetryFixture();
      f.root.Values.monitoring[field] = subjects;
      expect(() => render(f), JSON.stringify(subjects)).toThrow(`monitoring.${field}`);
    }
  });
  it.each(["oidc", "auth", "service-account", "credential", "pull-secret", "pull-policy", "headlamp-enabled", "monitoring-enabled",
    "monitoring-namespace", "monitoring-service", "prometheus", "sre", "funding", "budget", "source", "registration", "controller-uid", "receipt", "mutable-receipt"])(
    "does not relax unrelated authority or configuration: %s", fault => {
      const f = telemetryFixture();
      if (fault === "oidc") f.root.Values.headlamp.oidc.issuerURL = "https://unreviewed.example";
      if (fault === "auth") f.root.Values.headlamp.oidc.enabled = false;
      if (fault === "service-account") f.root.Values.headlamp.serviceAccountName = "unreviewed";
      if (fault === "credential") f.root.Values.headlamp.oidc.clientSecret.name = "different-credential";
      if (fault === "pull-secret") f.root.Values.headlamp.imagePullSecrets = [{ name: "different-registry-credential" }];
      if (fault === "pull-policy") f.root.Values.headlamp.image.pullPolicy = "Never";
      if (fault === "headlamp-enabled") f.root.Values.headlamp.enabled = false;
      if (fault === "monitoring-enabled") f.root.Values.monitoring.enabled = false;
      if (fault === "monitoring-namespace") f.root.Values.monitoring.namespaceOverride = "other";
      if (fault === "monitoring-service") f.root.Values.monitoring.fullnameOverride = "other";
      if (fault === "prometheus") f.root.Values.monitoring.prometheus.enabled = false;
      if (fault === "sre") f.root.Values.sre.enabled = false;
      if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
      if (fault === "budget") f.root.Values.inferenceBudget.enabled = false;
      if (fault === "source") f.source.spec.unreviewed = true;
      if (fault === "registration") f.registration.spec.controller.release = "other";
      if (fault === "controller-uid") f.controller.metadata.uid = "other";
      if (fault === "receipt") f.adoptionMap.data.intentUID = "other";
      if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
      expect(() => render(f)).toThrow();
    });
});
