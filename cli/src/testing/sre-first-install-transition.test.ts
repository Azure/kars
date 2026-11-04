// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";
import { adoptedFixture, render, hash } from "./sre-first-install-transition.test-support.js";

describe("SRE controller operation fences", () => {
  function fencedFixture() {
    const f = adoptedFixture();
    f.controller.metadata.resourceVersion = "123";
    f.root.Values.controller.upgradePrecondition = { uid: "controller-uid", resourceVersion: "123" };
    return f;
  }

  it("preserves the sealed adoption receipt while retaining the exact live operation fence", () => {
    const f = fencedFixture();
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.controller.upgradePrecondition).toEqual(f.root.Values.controller.upgradePrecondition);
  });

  it.each([
    null, [], "123", {}, { uid: "controller-uid" },
    { uid: "", resourceVersion: "123" }, { uid: "controller-uid", resourceVersion: "" },
    { uid: "controller-uid", resourceVersion: 123 }, { uid: 123, resourceVersion: "123" },
    { uid: "changed", resourceVersion: "123" }, { uid: "controller-uid", resourceVersion: "122" },
    { uid: "controller-uid", resourceVersion: "123", extra: true },
  ].map(fence => ({ fence })))("rejects invalid or stale fences: $fence", ({ fence }) => {
    const f = fencedFixture();
    f.root.Values.controller.upgradePrecondition = fence;
    expect(() => render(f)).toThrow(/upgradePrecondition/);
  });

  it.each(["adoption", "live-version", "credentials", "receipt"])("does not exempt %s changes", fault => {
    const f = fencedFixture();
    if (fault === "adoption") f.adoptionMap = {};
    if (fault === "live-version") delete f.controller.metadata.resourceVersion;
    if (fault === "credentials") f.root.Values.controller.extraEnv.push({ name: "AZURE_OPENAI_API_KEY", value: "changed" });
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "forged";
    expect(() => render(f)).toThrow();
  });
});

describe("SRE Kind kube-state-metrics capacity refresh", () => {
  function capacityFixture() {
    const f = adoptedFixture();
    f.root.Values.global = { installationProfile: "kind-beta" };
    f.root.Values.monitoring = { enabled: true, "kube-state-metrics": {
      resources: { requests: { cpu: "25m", memory: "32Mi" }, limits: { cpu: "250m", memory: "128Mi" } },
      image: { repository: "registry.example/metrics", tag: "latest" } } };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.monitoring.kubeStateMetricsResourcesBaseline = structuredClone(baseline.monitoring["kube-state-metrics"].resources);
    f.root.Values.monitoring["kube-state-metrics"].resources = {
      requests: { cpu: "25m", memory: "64Mi" }, limits: { cpu: "500m", memory: "256Mi" } };
    return f;
  }
  it("retains the sealed receipt while emitting the bounded selected resources", () => {
    const f = capacityFixture();
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.monitoring).toEqual(f.root.Values.monitoring);
    expect(f).toEqual(before);
  });
  it.each(["unadopted", "production", "disabled", "forged-baseline", "mutable-receipt", "baseline-type", "selected-type",
    "missing-limits", "extra-group", "group-type", "extra-resource", "numeric", "zero", "negative", "overflow", "unit",
    "request-exceeds-limit", "baseline-request-exceeds-limit", "decrease", "image", "probe", "credentials", "other-component"])(
    "rejects capacity refresh with %s", fault => {
      const f = capacityFixture();
      const monitoring = f.root.Values.monitoring;
      const metrics = monitoring["kube-state-metrics"];
      if (fault === "unadopted") f.adoptionMap = {};
      if (fault === "production") f.root.Values.global.installationProfile = "production";
      if (fault === "disabled") monitoring.enabled = false;
      if (fault === "forged-baseline") monitoring.kubeStateMetricsResourcesBaseline.requests.memory = "16Mi";
      if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
      if (fault === "baseline-type") monitoring.kubeStateMetricsResourcesBaseline = [];
      if (fault === "selected-type") metrics.resources = [];
      if (fault === "missing-limits") delete metrics.resources.limits;
      if (fault === "extra-group") metrics.resources.claims = [];
      if (fault === "group-type") metrics.resources.limits = "500m";
      if (fault === "extra-resource") metrics.resources.limits["nvidia.com/gpu"] = "1";
      if (fault === "numeric") metrics.resources.limits.cpu = 500;
      if (fault === "zero") metrics.resources.limits.cpu = "0m";
      if (fault === "negative") metrics.resources.limits.cpu = "-1m";
      if (fault === "overflow") metrics.resources.limits.cpu = "999999999999999999999m";
      if (fault === "unit") metrics.resources.limits.memory = "256MB";
      if (fault === "request-exceeds-limit") metrics.resources.requests.cpu = "501m";
      if (fault === "baseline-request-exceeds-limit") monitoring.kubeStateMetricsResourcesBaseline.requests.cpu = "251m";
      if (fault === "decrease") metrics.resources.requests.cpu = "24m";
      if (fault === "image") metrics.image.repository = "unreviewed";
      if (fault === "probe") metrics.livenessProbe = { enabled: false };
      if (fault === "credentials") metrics.extraEnv = [{ name: "TOKEN", value: "unreviewed" }];
      if (fault === "other-component") monitoring.prometheus = { enabled: false };
      expect(() => render(f)).toThrow();
    });
});

describe("SRE Kind metrics node address refresh", () => {
  function metricsFixture() {
    const f = adoptedFixture();
    f.root.Values.global = { installationProfile: "kind-beta" };
    f.root.Values.metricsServer = { enabled: true,
      hostAliases: [{ ip: "172.20.0.2", hostnames: ["kind-control-plane"] }] };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.metricsServer.hostAliasesBaseline = structuredClone(baseline.metricsServer.hostAliases);
    f.root.Values.metricsServer.hostAliases[0].ip = "172.20.0.3";
    f.metricsNodes = { "kind-control-plane": { apiVersion: "v1", kind: "Node",
      metadata: { name: "kind-control-plane", uid: "node-uid" },
      status: { addresses: [{ type: "InternalIP", address: "172.20.0.3" }] } } };
    return f;
  }
  it("retains the sealed receipt and only selects a live InternalIP for the original node hostname", () => {
    const f = metricsFixture();
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(doc => doc.toJSON()).filter(Boolean);
    expect(docs.find(doc => doc.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    const values = JSON.parse(docs.find(doc => doc.metadata.name === "selected-values").data["values.json"]);
    expect(values.metricsServer).toEqual(f.root.Values.metricsServer);
    expect(f).toEqual(before);
  });
  it.each(["unadopted", "profile", "disabled", "baseline", "baseline-type", "selected-type", "empty", "extra-alias",
    "hostname", "extra-hostname", "extra-field", "missing-node", "node-uid", "node-name", "node-kind", "deleting-node",
    "external-ip", "wrong-ip", "mutable-receipt", "other-settings"])("rejects metrics address refresh with %s", fault => {
    const f = metricsFixture();
    const metrics = f.root.Values.metricsServer;
    const node = f.metricsNodes["kind-control-plane"];
    if (fault === "unadopted") f.adoptionMap = {};
    if (fault === "profile") f.root.Values.global.installationProfile = "production";
    if (fault === "disabled") metrics.enabled = false;
    if (fault === "baseline") metrics.hostAliasesBaseline[0].ip = "172.20.0.4";
    if (fault === "baseline-type") metrics.hostAliasesBaseline = {};
    if (fault === "selected-type") metrics.hostAliases = {};
    if (fault === "empty") metrics.hostAliases = [];
    if (fault === "extra-alias") metrics.hostAliases.push(structuredClone(metrics.hostAliases[0]));
    if (fault === "hostname") metrics.hostAliases[0].hostnames[0] = "foreign.example";
    if (fault === "extra-hostname") metrics.hostAliases[0].hostnames.push("foreign.example");
    if (fault === "extra-field") metrics.hostAliases[0].bypass = true;
    if (fault === "missing-node") f.metricsNodes = {};
    if (fault === "node-uid") node.metadata.uid = "";
    if (fault === "node-name") node.metadata.name = "replacement";
    if (fault === "node-kind") node.kind = "Pod";
    if (fault === "deleting-node") node.metadata.deletionTimestamp = "2026-11-04T12:00:00Z";
    if (fault === "external-ip") node.status.addresses[0].type = "ExternalIP";
    if (fault === "wrong-ip") node.status.addresses[0].address = "172.20.0.4";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "other-settings") metrics.insecureSkipTLSVerify = true;
    expect(() => render(f)).toThrow();
  });
});
