// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";
import {
  topUp, adoptedFixture, hash, render, renderFunding, runtimeFixture, fixture,
} from "./sre-first-install-transition.test-support.js";

describe("adopted Core installed-image continuity", () => {
  const allowance = { id: "operator-unbounded-1", configUid: topUp.configUid, accountUid: topUp.accountUid,
    root: topUp.root, originalLimits: topUp.originalLimits, ledgerDigest: `sha256:${"a".repeat(64)}`, previousTokenTopUpIds: [topUp.id] };
  function retainedRouterFixture() {
    const f = adoptedFixture();
    f.root.Values.evals = { runnerImage: "ghcr.io/azure/kars-conformance-runner:latest" };
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.evals.runnerImageBaseline = f.root.Values.evals.runnerImage;
    f.root.Values.evals.runnerImage = `localhost:5077/new-runner:latest@sha256:${"c".repeat(64)}`;
    f.root.Values.sre.routerImageBaseline = `registry.example/router:latest@sha256:${"a".repeat(64)}`;
    f.root.Values.inferenceRouter.image = { repository: "localhost:5077/installed-router", tag: `latest@sha256:${"b".repeat(64)}` };
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [];
    f.root.Values.controller.image = { repository: "localhost:5077/installed-controller", tag: `latest@sha256:${"d".repeat(64)}`, digest: `sha256:${"d".repeat(64)}` };
    f.controller.metadata = { ...f.controller.metadata, name: "kars-controller", namespace: "kars-system" };
    f.controller.spec = { template: { spec: { containers: [{ name: "controller",
      image: `${f.root.Values.controller.image.repository}:${f.root.Values.controller.image.tag}`, env: [
        { name: "INFERENCE_ROUTER_IMAGE", value: `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}` },
        { name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: f.root.Values.inferenceBudget.routerImageDigest },
        { name: "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST", value: `sha256:${"a".repeat(64)}` },
      ] }] } } };
    return f;
  }
  it.each([false, true])("retains installed images, adoption and selected receipts during runner refresh (allowance: %s)", funded => {
    const f = retainedRouterFixture();
    if (funded) {
      f.root.Values.inferenceBudget.tokenTopUps = [topUp];
      f.root.Values.inferenceBudget.unboundedTokenAllowances = [allowance];
    }
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
    expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
    expect(f).toEqual(before);
    const emitted = renderFunding(funded ? [topUp] : [], true, funded ? [allowance] : [])
      .find(d => d.kind === "ConfigMap" && d.metadata.name === "kars-inference-budget-contracts");
    expect(JSON.parse(emitted.data["unbounded-token-allowances.json"])).toEqual(funded ? [allowance] : []);
    expect(JSON.parse(emitted.data["token-top-ups.json"])).toEqual(funded ? [topUp] : []);
  });
  it.each(["missing-controller", "controller-uid", "workspace-uid", "controller-name", "controller-namespace", "retiring",
    "duplicate-container", "missing-image", "changed-image", "duplicate-image", "secret-image", "changed-verifier", "missing-original-verifier",
    "mutable-selected", "mismatched-selected-digest", "forged-baseline-repository", "forged-baseline-digest", "receipt", "mutable-receipt",
    "funding", "credentials", "pull-policy", "source", "malformed-allowances", "unrelated-budget"])('rejects retained router continuity with %s', fault => {
    const f = retainedRouterFixture();
    const containers = f.controller.spec.template.spec.containers;
    const env = containers[0].env;
    if (fault === "missing-controller") delete f.controller.spec;
    if (fault === "controller-uid") f.controller.metadata.uid = "replacement";
    if (fault === "workspace-uid") f.workspace.metadata.uid = "replacement";
    if (fault === "controller-name") f.controller.metadata.name = "other";
    if (fault === "controller-namespace") f.controller.metadata.namespace = "other";
    if (fault === "retiring") f.controller.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "duplicate-container") containers.push(structuredClone(containers[0]));
    if (fault === "missing-image") env.splice(0, 1);
    if (fault === "changed-image") env[0].value = `foreign.example/router:latest@sha256:${"b".repeat(64)}`;
    if (fault === "duplicate-image") env.push(structuredClone(env[0]));
    if (fault === "secret-image") env[0] = { name: env[0].name, valueFrom: { secretKeyRef: { name: "image", key: "value" } } };
    if (fault === "changed-verifier") env[1].value = `sha256:${"c".repeat(64)}`;
    if (fault === "missing-original-verifier") env.pop();
    if (fault === "mutable-selected") f.root.Values.inferenceRouter.image.tag = "latest";
    if (fault === "mismatched-selected-digest") f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"c".repeat(64)}`;
    if (fault === "forged-baseline-repository") f.root.Values.sre.routerImageBaseline = `foreign.example/router:latest@sha256:${"a".repeat(64)}`;
    if (fault === "forged-baseline-digest") f.root.Values.sre.routerImageBaseline = `registry.example/router:latest@sha256:${"f".repeat(64)}`;
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "credentials") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    if (fault === "pull-policy") f.root.Values.inferenceRouter.image.pullPolicy = "Never";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "malformed-allowances") f.root.Values.inferenceBudget.unboundedTokenAllowances = { enabled: true };
    if (fault === "unrelated-budget") f.root.Values.inferenceBudget.disableAccounting = true;
    expect(() => render(f)).toThrow();
  });
  function retainedRuntimeFixture(changed: "openclaw" | "hermes" | "both" = "both") {
    const f = runtimeFixture();
    if (changed !== "hermes") f.root.Values.sandbox.image.repository = "localhost:5077/installed-openclaw";
    if (changed !== "openclaw") f.root.Values.runtimes.hermes.image = `localhost:5077/installed-hermes:latest@sha256:${"c".repeat(64)}`;
    f.controller.metadata = { ...f.controller.metadata, name: "kars-controller", namespace: "kars-system" };
    f.controller.spec = { template: { spec: { containers: [{ name: "controller", env: [
      { name: "SANDBOX_IMAGE", value: `${f.root.Values.sandbox.image.repository}:${f.root.Values.sandbox.image.tag}` },
      { name: "HERMES_RUNTIME_IMAGE", value: f.root.Values.runtimes.hermes.image },
    ] }] } } };
    return f;
  }
  it.each(["openclaw", "hermes", "both"] as const)("retains the exact installed %s repository migration without changing selected values", changed => {
    const f = retainedRuntimeFixture(changed);
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [allowance];
    const before = structuredClone(f);
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"])).toEqual(before.root.Values);
    expect(docs.filter(d => d.kind === "KarsSandbox")).toEqual(before.resources);
    expect(f).toEqual(before);
  });
  it.each(["missing-controller", "controller-uid", "workspace-uid", "controller-name", "controller-namespace", "retiring",
    "duplicate-container", "missing-openclaw", "missing-hermes", "different-openclaw", "different-hermes",
    "duplicate-openclaw", "duplicate-hermes", "indirect-openclaw", "indirect-hermes", "extra-image-field",
    "selected-openclaw", "selected-hermes", "mutable-openclaw", "mutable-hermes", "baseline-openclaw", "baseline-hermes",
    "baseline-digest", "receipt", "mutable-receipt", "funding", "runtime-env", "pull-policy", "source", "credentials"])("rejects retained runtime continuity with %s", fault => {
    const f = retainedRuntimeFixture();
    const containers = f.controller.spec.template.spec.containers;
    const env = containers[0].env;
    if (fault === "missing-controller") delete f.controller.spec;
    if (fault === "controller-uid") f.controller.metadata.uid = "replacement";
    if (fault === "workspace-uid") f.workspace.metadata.uid = "replacement";
    if (fault === "controller-name") f.controller.metadata.name = "other";
    if (fault === "controller-namespace") f.controller.metadata.namespace = "other";
    if (fault === "retiring") f.controller.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "duplicate-container") containers.push(structuredClone(containers[0]));
    if (fault === "missing-openclaw") env.splice(0, 1);
    if (fault === "missing-hermes") env.pop();
    if (fault === "different-openclaw") env[0].value = `foreign.example/openclaw:latest@sha256:${"b".repeat(64)}`;
    if (fault === "different-hermes") env[1].value = `foreign.example/hermes:latest@sha256:${"c".repeat(64)}`;
    if (fault === "duplicate-openclaw") env.push(structuredClone(env[0]));
    if (fault === "duplicate-hermes") env.push(structuredClone(env[1]));
    for (const [name, index] of [["indirect-openclaw", 0], ["indirect-hermes", 1]] as const) {
      if (fault === name) env[index] = { name: env[index].name, valueFrom: { secretKeyRef: { name: "image", key: "value" } } };
    }
    if (fault === "extra-image-field") env[0].valueFrom = { fieldRef: { fieldPath: "metadata.name" } };
    if (fault === "selected-openclaw") f.root.Values.sandbox.image.repository = "foreign.example/openclaw";
    if (fault === "selected-hermes") f.root.Values.runtimes.hermes.image = `foreign.example/hermes:latest@sha256:${"c".repeat(64)}`;
    if (fault === "mutable-openclaw") f.root.Values.sandbox.image.tag = "latest";
    if (fault === "mutable-hermes") f.root.Values.runtimes.hermes.image = "localhost:5077/installed-hermes:latest";
    if (fault === "baseline-openclaw") f.root.Values.sre.runtimeImageBaseline.openclaw = `foreign.example/openclaw:latest@sha256:${"a".repeat(64)}`;
    if (fault === "baseline-hermes") f.root.Values.sre.runtimeImageBaseline.hermes = `foreign.example/hermes:latest@sha256:${"a".repeat(64)}`;
    if (fault === "baseline-digest") f.root.Values.sre.runtimeImageBaseline.hermes = `registry.example/hermes:latest@sha256:${"f".repeat(64)}`;
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "runtime-env") f.root.Values.runtimes.hermes.extraEnv = { DISABLE_GOVERNANCE: "true" };
    if (fault === "pull-policy") f.root.Values.sandbox.image.pullPolicy = "Never";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "credentials") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    expect(() => render(f)).toThrow();
  });
  it("accepts only the empty new allowance default during legacy initial adoption", () => {
    const f = fixture();
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [];
    expect(render(f)).toContain("intentUID");
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [allowance];
    expect(() => render(f)).toThrow(/Unbounded token allowances require completed SRE adoption/);
  });
  it.each([{ sealed: [] }, { sealed: [allowance] }])("restores the exact originally sealed allowance list only for comparison (%j)", ({ sealed }) => {
    const f = adoptedFixture();
    f.root.Values.inferenceBudget.unboundedTokenAllowances = sealed;
    const intent = JSON.parse(f.intentMap.data["intent.json"]);
    intent.budget.unboundedTokenAllowances = sealed;
    f.intentMap.data["intent.json"] = JSON.stringify(intent);
    f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.inferenceBudget.unboundedTokenAllowances = [allowance];
    const docs = parseAllDocuments(render(f, true)).map(d => d.toJSON()).filter(Boolean);
    expect(docs.find(d => d.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(JSON.parse(docs.find(d => d.metadata.name === "selected-values").data["values.json"]).inferenceBudget.unboundedTokenAllowances).toEqual([allowance]);
  });
});
