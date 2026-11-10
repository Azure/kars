// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";
import { canonicalSchema } from "../lib/schema-documents.js";
import {
  fixture, render, adoptedFixture, routerFixture, topUp, hash, runtimeFixture, renderFunding,
  dispatcherFixture, dispatcherRefreshFixture,
} from "./sre-first-install-transition.test-support.js";

describe("ordinary SRE ownership transition", () => {
  it("adopts the exact original unbounded authority without inventing tokens or expiry", () => {
    const f = fixture(1000, undefined, undefined, true);
    expect(() => render(f)).not.toThrow();
    expect(f.registration.spec.firstInstallFunding.grant).not.toHaveProperty("expiresAt");
    f.registration.spec.firstInstallFunding.grant.unboundedTokens = false;
    expect(() => render(f)).toThrow();
  });
  it.each(["UTC", "Etc/GMT-2", "Etc/GMT+7"])("honors UTC funding expiry in %s", timeZone => {
    for (const adopted of [false, true]) {
      for (const minutes of [-30, 30]) {
        const expiresAt = new Date(Date.now() + minutes * 60_000).toISOString().replace(/\.\d{3}Z$/, "Z");
        const f = adopted ? adoptedFixture(undefined, expiresAt) : fixture(1000, undefined, expiresAt);
        if (minutes > 0 || adopted) expect(() => render(f, false, timeZone)).not.toThrow();
        else expect(() => render(f, false, timeZone)).toThrow(/initial adoption also requires unexpired authority/);
      }
    }
  });
  it("renders an expired adopted repair without rewriting funding or manufacturing renewal authority", () => {
    const f = adoptedFixture(undefined, "2020-01-01T00:00:00Z");
    f.registration.status.phase = "Blocked";
    delete f.registration.status.privacyRevision;
    const before = structuredClone(f);
    const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(documents.some(document => document.kind === "KarsSRERegistration")).toBe(false);
    expect(JSON.parse(documents.find(document => document.metadata.name === "selected-values").data["values.json"]).inferenceBudget.tokenTopUps).toBeUndefined();
    expect(f).toEqual(before);
  });
  it.each([false, true])("allows adopted controller repair without granting blocked authority (cleared qualification: %s)", cleared => {
    const f = routerFixture();
    f.registration.status.phase = "Blocked";
    if (cleared) delete f.registration.status.privacyRevision;
    f.root.Values.controller.image.tag = `latest@sha256:${"c".repeat(64)}`;
    f.root.Values.controller.image.digest = `sha256:${"c".repeat(64)}`;
    const before = structuredClone(f);
    const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "verified").data).toEqual(before.adoptionMap.data);
    expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(before.resources);
    expect(documents.some(document => document.kind === "KarsSRERegistration")).toBe(false);
    expect(f).toEqual(before);
  });
  it.each([false, true])("requires qualified privacy for Ready authority (adopted: %s)", adopted => {
    const f = adopted ? adoptedFixture() : fixture();
    delete f.registration.status.privacyRevision;
    expect(() => render(f)).toThrow(/live original identities/);
  });
  it.each([false, true])("rejects unknown or non-current authority during recovery (adopted: %s)", adopted => {
    for (const phase of ["", "Pending", "Retired", "Unknown", ...(adopted ? [] : ["Blocked"])]) {
      const f = adopted ? adoptedFixture() : fixture();
      f.registration.status.phase = phase;
      expect(() => render(f)).toThrow(/live original identities/);
    }
  });
  it.each([
    "receipt-uid", "receipt-owner", "receipt-digest", "mutable-receipt", "deleted-receipt",
    "intent-uid", "intent-digest", "mutable-intent", "deleted-intent", "source-uid",
    "controller-uid", "runtime-uid", "workspace-uid", "privacy-revision", "generation",
    "disabled", "funding", "expiry", "values",
  ])("rejects changed %s while adopted SRE is blocked", fault => {
    const f = routerFixture();
    f.registration.status.phase = "Blocked";
    if (fault === "receipt-uid") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "receipt-owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "receipt-digest") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "deleted-receipt") f.adoptionMap.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "intent-uid") f.intentMap.metadata.uid = "replacement";
    if (fault === "intent-digest") f.registration.spec.firstInstallFunding.intent.digest = "f".repeat(64);
    if (fault === "mutable-intent") f.intentMap.immutable = false;
    if (fault === "deleted-intent") f.intentMap.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "source-uid") f.source.metadata.uid = "replacement";
    if (fault === "controller-uid") f.controller.metadata.uid = "replacement";
    if (fault === "runtime-uid") f.runtime.metadata.uid = "replacement";
    if (fault === "workspace-uid") f.workspace.metadata.uid = "replacement";
    if (fault === "privacy-revision") f.registration.status.privacyRevision = "unknown";
    if (fault === "generation") f.registration.status.observedGeneration = 0;
    if (fault === "disabled") f.registration.spec.enabled = false;
    if (fault === "funding") f.registration.spec.firstInstallFunding.grant.limits.tokens += 1;
    if (fault === "expiry") f.registration.spec.firstInstallFunding.grant.expiresAt = "2999-02-01T00:00:00Z";
    if (fault === "values") f.root.Values.controller.extraEnv.push({ name: "UNREVIEWED", value: "true" });
    expect(() => render(f)).toThrow();
  });
  it("keeps the original runtime digest rule without an explicit upgrade baseline", () => {
    const documents = parseAllDocuments(render(adoptedFixture(), true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "controller-env").spec.containers[0].env).toEqual([
      { name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: `sha256:${"a".repeat(64)}` },
    ]);
  });
  it.each([false, true])("refreshes only the router tag/digest against the original sealed baseline (composed: %s)", composed => {
    const f = routerFixture(composed);
    const original = structuredClone(f);
    if (composed) {
      f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
      f.root.Values.controller.image.tag = `latest@sha256:${"c".repeat(64)}`;
      f.root.Values.controller.image.digest = `sha256:${"c".repeat(64)}`;
    }
    for (const digit of ["b", "c", "c"]) {
      f.root.Values.inferenceRouter.image.tag = `latest@sha256:${digit.repeat(64)}`;
      f.root.Values.inferenceBudget.routerImageDigest = `sha256:${digit.repeat(64)}`;
      for (const grants of [[], [topUp]]) {
        f.root.Values.inferenceBudget.tokenTopUps = grants;
        const selected = structuredClone(f.root.Values);
        const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
        expect(documents.find(document => document.metadata.name === "verified").data).toEqual(original.adoptionMap.data);
        expect(documents.find(document => document.metadata.name === "controller-env").spec.containers[0].env).toEqual([
          { name: "KARS_INFERENCE_BUDGET_ROUTER_DIGEST", value: selected.inferenceBudget.routerImageDigest },
          { name: "KARS_INFERENCE_BUDGET_FIRST_INSTALL_ROUTER_DIGEST", value: `sha256:${"a".repeat(64)}` },
        ]);
        expect(JSON.parse(documents.find(document => document.metadata.name === "selected-values").data["values.json"])).toEqual(selected);
        expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(original.resources);
        expect(f.intentMap).toEqual(original.intentMap);
        expect(f.registration).toEqual(original.registration);
        expect(f.source).toEqual(original.source);
        expect(f.root.Values.sre.runtimeImageBaseline).toEqual(original.root.Values.sre.runtimeImageBaseline);
      }
    }
  });
  it.each(["matching", "stale", "removed"])("preserves an originally explicit router digest during refresh: %s", mode => {
    const f = adoptedFixture();
    f.root.Values.inferenceRouter.image.digest = `sha256:${"a".repeat(64)}`;
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.sre.routerImageBaseline = `${f.root.Values.inferenceRouter.image.repository}:${f.root.Values.inferenceRouter.image.tag}`;
    f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    if (mode === "matching") f.root.Values.inferenceRouter.image.digest = f.root.Values.inferenceBudget.routerImageDigest;
    if (mode === "removed") delete f.root.Values.inferenceRouter.image.digest;
    if (mode !== "matching") {
      expect(() => render(f)).toThrow();
      return;
    }
    const documents = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
    expect(JSON.parse(documents.find(document => document.metadata.name === "selected-values").data["values.json"]).inferenceRouter.image.digest)
      .toBe(`sha256:${"b".repeat(64)}`);
  });
  it("does not introduce the router baseline during initial ownership adoption", () => {
    expect(() => render(routerFixture(false, false))).toThrow(/requires completed SRE adoption/);
  });
  it.each([
    "missing-baseline", "forged-baseline", "mutable-baseline", "baseline-type", "empty-baseline",
    "mutable-selected", "wrong-tag", "mismatched-digest", "repository", "forged-repository-pair",
    "extra-image-field", "pull-policy", "router-env", "runtime-env", "runtime-baseline", "catalog", "tls",
    "built-in-funding", "original-tokens", "original-expiry", "bound-grant", "bound-expiry", "credential",
    "receipt-digest", "receipt-uid", "mutable-receipt", "receipt-owner", "source", "source-spec", "source-label",
    "intent-uid", "intent-digest", "intent-mutable", "status", "retired", "controller", "runtime", "workspace", "hook",
  ])("rejects an explicitly requested router transition with changed %s", fault => {
    const f = routerFixture(true);
    const values = f.root.Values;
    values.inferenceBudget.tokenTopUps = [topUp];
    if (fault === "missing-baseline") delete values.sre.routerImageBaseline;
    if (fault === "forged-baseline") values.sre.routerImageBaseline = `registry.example/router:latest@sha256:${"f".repeat(64)}`;
    if (fault === "mutable-baseline") values.sre.routerImageBaseline = "registry.example/router:latest";
    if (fault === "baseline-type") values.sre.routerImageBaseline = { image: values.sre.routerImageBaseline };
    if (fault === "empty-baseline") values.sre.routerImageBaseline = "";
    if (fault === "mutable-selected") values.inferenceRouter.image.tag = "latest";
    if (fault === "wrong-tag") values.inferenceRouter.image.tag = `v2@sha256:${"b".repeat(64)}`;
    if (fault === "mismatched-digest") values.inferenceBudget.routerImageDigest = `sha256:${"c".repeat(64)}`;
    if (fault === "repository") values.inferenceRouter.image.repository = "foreign.example/router";
    if (fault === "forged-repository-pair") {
      values.inferenceRouter.image.repository = "foreign.example/router";
      values.sre.routerImageBaseline = `foreign.example/router:latest@sha256:${"a".repeat(64)}`;
    }
    if (fault === "extra-image-field") values.inferenceRouter.image.digest = `sha256:${"b".repeat(64)}`;
    if (fault === "pull-policy") values.inferenceRouter.image.pullPolicy = "Always";
    if (fault === "router-env") values.inferenceRouter.extraEnv = [{ name: "SKIP_AUTH", value: "true" }];
    if (fault === "runtime-env") values.runtimes.hermes.extraEnv = { SKIP_AUTH: "true" };
    if (fault === "runtime-baseline") values.sre.runtimeImageBaseline.hermes = values.runtimes.hermes.image;
    if (fault === "catalog") values.inferenceBudget.catalogVersion = "replacement";
    if (fault === "tls") values.inferenceBudget.tls = { certificate: "replacement" };
    if (fault === "built-in-funding") values.inferenceBudget.builtInFunding = [{ tokens: 9000 }];
    if (fault === "original-tokens") values.sre.firstInstallFunding.tokens++;
    if (fault === "original-expiry") values.sre.firstInstallFunding.expiresAt = "2999-02-01T00:00:00Z";
    if (fault === "bound-grant") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
    if (fault === "bound-expiry") f.registration.spec.firstInstallFunding.grant.expiresAt = "2000-01-01T00:00:00Z";
    if (fault === "credential") values.sre.existingSecret = "replacement";
    if (fault === "receipt-digest") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "receipt-uid") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "receipt-owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "source-spec") f.source.spec.runtime = "replacement";
    if (fault === "source-label") {
      f.resources[0].metadata.labels = { "kars.azure.com/role": "sre" };
      f.source.metadata.labels = { "kars.azure.com/role": "replacement" };
    }
    if (fault === "intent-uid") f.intentMap.metadata.uid = "replacement";
    if (fault === "intent-digest") f.registration.spec.firstInstallFunding.intent.digest = "f".repeat(64);
    if (fault === "intent-mutable") f.intentMap.immutable = false;
    if (fault === "status") f.registration.status.observedGeneration = 0;
    if (fault === "retired") f.registration.spec.enabled = false;
    if (fault === "controller") f.controller.metadata.uid = "replacement";
    if (fault === "runtime") f.runtime.metadata.uid = "replacement";
    if (fault === "workspace") f.workspace.metadata.uid = "replacement";
    if (fault === "hook") values.schemaHook.enabled = true;
    expect(() => render(f)).toThrow();
  });
  it.each([{ baseline: undefined }, { baseline: [] }])("keeps the original receipt and funding across empty and published top-ups (sealed $baseline)", ({ baseline }) => {
    const f = adoptedFixture(baseline);
    const original = structuredClone(f);
    for (const grants of [[], [topUp], [topUp, { ...topUp, id: "operator-capacity-2", tokens: 7000 }]]) {
      f.root.Values.inferenceBudget.tokenTopUps = grants;
      for (let replay = 0; replay < 2; replay++) {
        const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
        expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(original.adoptionMap.data);
        expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(original.resources);
      }
      expect(f.intentMap).toEqual(original.intentMap);
      expect(f.registration).toEqual(original.registration);
      expect(f.source).toEqual(original.source);
    }
  });
  it("permits the new empty default during legacy initial adoption, but not new funding", () => {
    const f = fixture();
    f.root.Values.inferenceBudget.tokenTopUps = [];
    const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(adoptedFixture().adoptionMap.data);
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    expect(() => render(f)).toThrow(/Token top-ups require completed SRE adoption/);
  });
  it("composes top-ups with only the already supported image, pull and dispatcher transitions", () => {
    const f = runtimeFixture(true, true);
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
    const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
  });
  it.each(["original-tokens", "original-expiry", "bound-grant", "catalog", "router", "tls", "built-in-funding", "receipt", "source", "schema-hook", "other-key", "malformed-list"])("does not conceal changed %s alongside a top-up", fault => {
    const f = adoptedFixture();
    f.root.Values.inferenceBudget.tokenTopUps = [topUp];
    if (fault === "original-tokens") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "original-expiry") f.root.Values.sre.firstInstallFunding.expiresAt = "2999-02-01T00:00:00Z";
    if (fault === "bound-grant") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
    if (fault === "catalog") f.root.Values.inferenceBudget.catalogVersion = "changed";
    if (fault === "router") f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    if (fault === "tls") f.root.Values.inferenceBudget.tls = { certificate: "changed" };
    if (fault === "built-in-funding") f.root.Values.inferenceBudget.builtInFunding = [{ tokens: 9000 }];
    if (fault === "receipt") f.adoptionMap.data.valuesDigest = "f".repeat(64);
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "schema-hook") f.root.Values.schemaHook.enabled = true;
    if (fault === "other-key") f.root.Values.inferenceBudget.ignoreAccounting = true;
    if (fault === "malformed-list") f.root.Values.inferenceBudget.tokenTopUps = { tokens: 5000 };
    expect(() => render(f)).toThrow();
  });
  it("emits the actual authorization list and retains the separate fund admission permission", () => {
    for (const grants of [[], [topUp]]) {
      const documents = renderFunding(grants);
      const config = documents.find(document => document.kind === "ConfigMap" && document.metadata.name === "kars-inference-budget-contracts");
      expect(JSON.parse(config.data["token-top-ups.json"])).toEqual(grants);
      const policy = documents.find(document => document.kind === "ValidatingAdmissionPolicy" && document.metadata.name === "kars-inference-budget-token-top-ups");
      expect(policy.spec.failurePolicy).toBe("Fail");
      expect(policy.spec.matchConstraints.resourceRules).toEqual([{
        apiGroups: [""], apiVersions: ["v1"], operations: ["CREATE", "UPDATE", "DELETE"], resources: ["configmaps"],
      }]);
      expect(policy.spec.validations.map((validation: any) => validation.expression)).toEqual([
        "((variables.oldFunding == variables.newFunding || (variables.oldFunding == '' && variables.newFunding == '[]')) && (variables.oldUnbounded == variables.newUnbounded || (variables.oldUnbounded == '' && variables.newUnbounded == '[]')) && variables.oldCalendarPolicy == variables.newCalendarPolicy) || authorizer.group('kars.azure.com').resource('karsbudgetaccounts').namespace(request.namespace).check('fund').allowed()",
      ]);
      const binding = documents.find(document => document.kind === "ValidatingAdmissionPolicyBinding" && document.spec.policyName === policy.metadata.name);
      expect(binding.spec.validationActions).toContain("Deny");
      for (const role of documents.filter(document => ["Role", "ClusterRole"].includes(document.kind))) {
        expect((role.rules ?? []).some((rule: any) => rule.verbs?.includes("fund"))).toBe(false);
      }
    }
    expect(() => renderFunding([topUp], false)).toThrow(/inferenceBudget.tokenTopUps requires inferenceBudget.enabled/);
  });
  it.each([
    { dispatcher: false, coverage: false }, { dispatcher: false, coverage: true },
    { dispatcher: true, coverage: false }, { dispatcher: true, coverage: true },
  ])("refreshes pinned runtime images against the sealed baseline ($dispatcher / $coverage)", ({ dispatcher, coverage }) => {
    const f = runtimeFixture(true, dispatcher);
    if (coverage) f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example,localhost:5077" });
    for (const digest of ["b", "d"]) {
      f.root.Values.sandbox.image.tag = `latest@sha256:${digest.repeat(64)}`;
      const docs = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
      expect(docs.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
      expect(docs.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
    }
  });
  it("refreshes explicit runtime digests while preserving the sealed baseline and field shape", () => {
    const f = runtimeFixture(true, false, true);
    const baseline = structuredClone(f.root.Values.sre.runtimeImageBaseline);
    for (const digest of ["b", "d"]) {
      f.root.Values.sandbox.image.tag = `latest@sha256:${digest.repeat(64)}`;
      f.root.Values.sandbox.image.digest = `sha256:${digest.repeat(64)}`;
      const docs = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
      expect(docs.find(document => document.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
      const values = JSON.parse(docs.find(document => document.metadata.name === "selected-values").data["values.json"]);
      expect(values.sandbox.image).toEqual(f.root.Values.sandbox.image);
      expect(values.sre.runtimeImageBaseline).toEqual(baseline);
      expect(docs.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
    }
  });
  it.each(["stale", "empty", "type", "removed", "added", "forged-baseline"])("rejects runtime digest drift: %s", fault => {
    const f = runtimeFixture(true, false, fault !== "added");
    const image = f.root.Values.sandbox.image;
    if (fault === "stale") image.digest = `sha256:${"a".repeat(64)}`;
    if (fault === "empty") image.digest = "";
    if (fault === "type") image.digest = { digest: `sha256:${"b".repeat(64)}` };
    if (fault === "removed") delete image.digest;
    if (fault === "added") image.digest = `sha256:${"b".repeat(64)}`;
    if (fault === "forged-baseline") f.root.Values.sre.runtimeImageBaseline.openclaw = `registry.example/openclaw:latest@sha256:${"f".repeat(64)}`;
    expect(() => render(f)).toThrow();
  });
  it("does not refresh runtime images during initial adoption", () => {
    expect(() => render(runtimeFixture(false))).toThrow();
  });
  it.each(["missing-baseline", "forged-baseline", "extra-baseline-field", "mutable-baseline", "baseline-type", "openclaw-mutable", "hermes-mutable", "openclaw-repository", "hermes-repository", "pull-policy", "runtime-env", "router", "funding", "source", "policy", "receipt", "mutable-receipt", "owner", "hook", "controller-env"])('rejects runtime image refresh with changed %s', fault => {
    const f = runtimeFixture();
    if (fault === "missing-baseline") delete f.root.Values.sre.runtimeImageBaseline;
    if (fault === "forged-baseline") f.root.Values.sre.runtimeImageBaseline.openclaw = `registry.example/openclaw:latest@sha256:${"f".repeat(64)}`;
    if (fault === "extra-baseline-field") f.root.Values.sre.runtimeImageBaseline.router = "ignored";
    if (fault === "mutable-baseline") f.root.Values.sre.runtimeImageBaseline.hermes = "registry.example/hermes:latest";
    if (fault === "baseline-type") f.root.Values.sre.runtimeImageBaseline = "invalid";
    if (fault === "openclaw-mutable") f.root.Values.sandbox.image.tag = "latest";
    if (fault === "hermes-mutable") f.root.Values.runtimes.hermes.image = "registry.example/hermes:latest";
    if (fault === "openclaw-repository") f.root.Values.sandbox.image.repository = "foreign.example/openclaw";
    if (fault === "hermes-repository") f.root.Values.runtimes.hermes.image = `foreign.example/hermes:latest@sha256:${"c".repeat(64)}`;
    if (fault === "pull-policy") f.root.Values.sandbox.image.pullPolicy = "Always";
    if (fault === "runtime-env") f.root.Values.runtimes.hermes.extraEnv = { DISABLE_GOVERNANCE: "true" };
    if (fault === "router") f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "policy") f.resources[0].spec.policyProfile = "changed";
    if (fault === "receipt") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "hook") f.root.Values.schemaHook.enabled = true;
    if (fault === "controller-env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    expect(() => render(f)).toThrow();
  });
  it.each([false, true])("activates the pinned Kind dispatcher preserving receipt and source (new pull coverage: %s)", coverage => {
    const f = dispatcherFixture();
    if (coverage) f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
    for (let attempt = 0; attempt < 2; attempt++) {
      const docs = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
      expect(docs.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
      expect(docs.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
    }
  });
  it("preserves pull coverage already sealed before dispatcher activation", () => {
    const f = dispatcherFixture();
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example" });
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    baseline.missionDispatcher = { enabled: false, authMode: "", image: {
      repository: "karsacr.azurecr.io/kars-mission-dispatcher", digest: "" }, relayUrl: "", registryUrl: "" };
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    expect(render(f)).toContain("name: verified");
    f.root.Values.controller.extraEnv[0].value = "localhost:5077";
    expect(() => render(f)).toThrow();
  });
  it("does not replace dispatcher configuration already bound into the adoption receipt", () => {
    const f = dispatcherFixture();
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    expect(render(f)).toContain("name: verified");
    f.root.Values.missionDispatcher.image.digest = `sha256:${"d".repeat(64)}`;
    expect(() => render(f)).toThrow();
  });
  it.each([
    { tag: false, authMode: "development" }, { tag: true, authMode: "development" },
    { tag: false, authMode: "entra" }, { tag: true, authMode: "entra" },
  ])("refreshes an originally sealed dispatcher image without changing authority ($tag / $authMode)", ({ tag, authMode }) => {
    const f = dispatcherRefreshFixture(tag, authMode);
    const original = structuredClone(f.root.Values.missionDispatcher.imageBaseline);
    for (const digest of ["d", "e"]) {
      const mission = f.root.Values.missionDispatcher;
      mission.image.digest = `sha256:${digest.repeat(64)}`;
      mission.image.repository = `localhost:5077/qualified-${digest}/dispatcher`;
      if (tag) mission.image.tag = `latest@${mission.image.digest}`;
      const docs = parseAllDocuments(render(f, true)).map(document => document.toJSON()).filter(Boolean);
      expect(docs.find(document => document.metadata.name === "verified").data).toEqual(f.adoptionMap.data);
      expect(docs.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
      const values = JSON.parse(docs.find(document => document.metadata.name === "selected-values").data["values.json"]);
      expect(values.missionDispatcher).toEqual(mission);
      expect(values.missionDispatcher.imageBaseline).toEqual(original);
    }
  });
  it("composes sealed dispatcher refresh with controller, runtime and pull-coverage changes", () => {
    const f = dispatcherRefreshFixture();
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    baseline.missionDispatcher.image = baseline.missionDispatcher.imageBaseline;
    delete baseline.missionDispatcher.imageBaseline;
    baseline.sandbox = { image: { repository: "registry.example/openclaw", tag: `latest@sha256:${"a".repeat(64)}` } };
    baseline.runtimes = { hermes: { image: `registry.example/hermes:latest@sha256:${"a".repeat(64)}` } };
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    f.root.Values.sandbox = structuredClone(baseline.sandbox);
    f.root.Values.runtimes = structuredClone(baseline.runtimes);
    f.root.Values.sre.runtimeImageBaseline = {
      openclaw: `${baseline.sandbox.image.repository}:${baseline.sandbox.image.tag}`, hermes: baseline.runtimes.hermes.image,
    };
    f.root.Values.sandbox.image.tag = `latest@sha256:${"b".repeat(64)}`;
    f.root.Values.runtimes.hermes.image = `registry.example/hermes:latest@sha256:${"b".repeat(64)}`;
    f.root.Values.controller.image.digest = `sha256:${"f".repeat(64)}`;
    f.root.Values.controller.image.tag = `latest@${f.root.Values.controller.image.digest}`;
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example" });
    expect(render(f)).toContain("name: verified");
  });
  it.each(["missing-baseline", "forged-baseline", "last-selected-baseline", "baseline-type", "baseline-mutable",
    "extra-baseline-field", "baseline-tag", "baseline-repository-type", "selected-type", "selected-mutable", "selected-tag", "extra-image-field",
    "repository", "tag-shape", "disabled", "auth", "relay", "registry", "extra-field", "funding", "env", "source", "account", "receipt", "mutable-receipt", "unadopted"])("rejects sealed dispatcher refresh with %s", fault => {
    const f = dispatcherRefreshFixture(true);
    const mission = f.root.Values.missionDispatcher;
    if (fault === "missing-baseline") delete mission.imageBaseline;
    if (fault === "forged-baseline") mission.imageBaseline.digest = `sha256:${"f".repeat(64)}`;
    if (fault === "last-selected-baseline") mission.imageBaseline = structuredClone(mission.image);
    if (fault === "baseline-type") mission.imageBaseline = "invalid";
    if (fault === "baseline-mutable") mission.imageBaseline.digest = "latest";
    if (fault === "extra-baseline-field") mission.imageBaseline.bypass = true;
    if (fault === "baseline-tag") mission.imageBaseline.tag = "latest";
    if (fault === "baseline-repository-type") mission.imageBaseline.repository = [];
    if (fault === "selected-type") mission.image = "invalid";
    if (fault === "selected-mutable") mission.image.digest = "";
    if (fault === "selected-tag") mission.image.tag = "latest";
    if (fault === "extra-image-field") mission.image.bypass = true;
    if (fault === "repository") mission.image.repository = "foreign.example/dispatcher";
    if (fault === "tag-shape") delete mission.image.tag;
    if (fault === "disabled") mission.enabled = false;
    if (fault === "auth") mission.authMode = "entra";
    if (fault === "relay") mission.relayUrl = "wss://foreign.example";
    if (fault === "registry") mission.registryUrl = "https://foreign.example";
    if (fault === "extra-field") mission.bypass = true;
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "account") delete f.source.spec.inferenceBudgetRef;
    if (fault === "receipt") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "unadopted") f.adoptionMap = {};
    expect(() => render(f)).toThrow();
  });
  it("does not activate a dispatcher during initial SRE adoption", () => {
    expect(() => render(dispatcherFixture(false))).toThrow();
  });
  it.each(["profile", "auth", "mutable-image", "repository", "relay", "registry", "extra-field", "extra-image-field", "funding", "router", "env", "source", "account", "receipt", "mutable-receipt"])("rejects dispatcher activation with changed %s", fault => {
    const f = dispatcherFixture();
    const mission = f.root.Values.missionDispatcher;
    if (fault === "profile") f.root.Values.global.installationProfile = "";
    if (fault === "auth") mission.authMode = "entra";
    if (fault === "mutable-image") mission.image.digest = "";
    if (fault === "repository") mission.image.repository = "";
    if (fault === "relay") mission.relayUrl = "ws://foreign.example";
    if (fault === "registry") mission.registryUrl = "http://foreign.example";
    if (fault === "extra-field") mission.bypass = true;
    if (fault === "extra-image-field") mission.image.tag = "latest";
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "router") f.root.Values.inferenceRouter.image.tag = "latest";
    if (fault === "env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "account") delete f.source.spec.inferenceBudgetRef;
    if (fault === "receipt") f.adoptionMap.data.intentUID = "replacement";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    expect(() => render(f)).toThrow();
  });
  it("allows only the original funded authority with schemaHook disabled and records adoption", () => {
    const f = fixture();
    const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
    const receipt = documents.find(document => document.kind === "ConfigMap");
    expect(receipt.data).toEqual(adoptedFixture().adoptionMap.data);
    expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
  });
  it("allows a controller image refresh only after the unchanged ownership transition", () => {
    for (const adopted of [false, true]) {
      const f = adopted ? adoptedFixture() : fixture();
      f.root.Values.controller.image.digest = `sha256:${"b".repeat(64)}`;
      f.root.Values.controller.image.tag = `latest@${f.root.Values.controller.image.digest}`;
      if (adopted) expect(render(f)).toContain("name: verified");
      else expect(() => render(f)).toThrow();
    }
  });
  it("preserves the release receipt and source on repeated ordinary upgrades", () => {
    const f = adoptedFixture();
    const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
    expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
    expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
  });
  it("adds literal pull coverage after adoption without changing the receipt or funded source", () => {
    const f = adoptedFixture();
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
    for (let attempt = 0; attempt < 2; attempt++) {
      const documents = parseAllDocuments(render(f)).map(document => document.toJSON()).filter(Boolean);
      expect(documents.find(document => document.kind === "ConfigMap").data).toEqual(f.adoptionMap.data);
      expect(documents.filter(document => document.kind === "KarsSandbox")).toEqual(f.resources);
    }
  });
  it("does not add pull coverage during the initial ownership transition", () => {
    const f = fixture();
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" });
    expect(() => render(f)).toThrow();
  });
  it.each(["duplicate", "valueFrom", "empty", "non-string", "extra-field", "other-env", "router", "funding", "source", "receipt-owner", "mutable-receipt"])("rejects pull coverage with changed %s", fault => {
    const f = adoptedFixture();
    const coverage: any = { name: "IMAGE_PULL_REGISTRIES", value: "localhost:5077" };
    f.root.Values.controller.extraEnv.push(coverage);
    if (fault === "duplicate") f.root.Values.controller.extraEnv.push({ ...coverage });
    if (fault === "valueFrom") {
      delete coverage.value;
      coverage.valueFrom = { configMapKeyRef: { name: "coverage", key: "registries" } };
    }
    if (fault === "empty") coverage.value = "";
    if (fault === "non-string") coverage.value = 5077;
    if (fault === "extra-field") coverage.valueFrom = {};
    if (fault === "other-env") f.root.Values.controller.extraEnv.push({ name: "DISABLE_GOVERNANCE", value: "true" });
    if (fault === "router") f.root.Values.inferenceRouter.image.tag = "latest";
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "receipt-owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    expect(() => render(f)).toThrow();
  });
  it("does not replace a pull declaration already bound into the adoption receipt", () => {
    const f = adoptedFixture();
    f.root.Values.controller.extraEnv.push({ name: "IMAGE_PULL_REGISTRIES", value: "registry.example" });
    const baseline = structuredClone(f.root.Values);
    delete baseline.controller.image;
    f.adoptionMap.data.valuesDigest = hash(canonicalSchema(baseline));
    expect(render(f)).toContain("name: verified");
    f.root.Values.controller.extraEnv[0].value = "localhost:5077";
    expect(() => render(f)).toThrow();
  });
  it.each(["receipt", "missing-values", "mutable-receipt", "receipt-owner", "deleting-receipt", "live-spec", "live-label", "values", "router", "catalog", "funding", "policy", "source", "intent", "controller", "runtime", "workspace", "retired", "status", "binding", "hook"])("keeps adopted upgrades fail-closed for changed %s", fault => {
    const f = adoptedFixture();
    if (fault === "receipt") f.adoptionMap.data.intentUID = "foreign";
    if (fault === "missing-values") delete f.adoptionMap.data.valuesDigest;
    if (fault === "mutable-receipt") f.adoptionMap.immutable = false;
    if (fault === "receipt-owner") f.adoptionMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    if (fault === "deleting-receipt") f.adoptionMap.metadata.deletionTimestamp = "2026-01-01T00:00:00Z";
    if (fault === "live-spec") f.source.spec.runtime = "changed";
    if (fault === "live-label") {
      f.resources[0].metadata.labels = { "kars.azure.com/role": "sre" };
      const intent = JSON.parse(f.intentMap.data["intent.json"]);
      intent.resources = f.resources;
      f.intentMap.data["intent.json"] = JSON.stringify(intent);
      f.registration.spec.firstInstallFunding.intent.digest = hash(f.intentMap.data["intent.json"]);
      f.source.metadata.labels = { "kars.azure.com/role": "changed" };
    }
    if (fault === "values") f.root.Values.sre.model = "changed";
    if (fault === "router") f.root.Values.inferenceRouter.image.tag = `latest@sha256:${"b".repeat(64)}`;
    if (fault === "catalog") f.root.Values.inferenceBudget.catalogVersion = "replacement";
    if (fault === "funding") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
    if (fault === "policy") f.resources[0].spec.policyProfile = "changed";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "intent") f.intentMap.metadata.uid = "replacement";
    if (fault === "controller") f.controller.metadata.uid = "replacement";
    if (fault === "runtime") f.runtime.metadata.uid = "replacement";
    if (fault === "workspace") f.workspace.metadata.uid = "replacement";
    if (fault === "retired") f.registration.spec.enabled = false;
    if (fault === "status") f.registration.status.observedGeneration = 0;
    if (fault === "binding") delete f.source.spec.inferenceBudgetRef;
    if (fault === "hook") f.root.Values.schemaHook.enabled = true;
    expect(() => render(f)).toThrow();
  });
  it("preserves million-token funding across JSON and Kubernetes numeric representations", () => {
    expect(render(fixture(1500000))).toContain("name: verified");
    const changed = fixture(1500000);
    changed.registration.spec.firstInstallFunding.grant.limits.tokens++;
    expect(() => render(changed)).toThrow();
  });
  it("rejects router changes that require migration of the sealed first-install contract", () => {
    const f = fixture();
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceRouter.image.tag = `latest@${f.root.Values.inferenceBudget.routerImageDigest}`;
    expect(() => render(f)).toThrow();
  });
  it.each(["mismatch", "mutable", "repository", "catalog", "funding", "source", "status"])("rejects router refresh with changed %s", fault => {
    const f = fixture();
    f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"b".repeat(64)}`;
    f.root.Values.inferenceRouter.image.tag = `latest@${f.root.Values.inferenceBudget.routerImageDigest}`;
    if (fault === "mismatch") f.root.Values.inferenceBudget.routerImageDigest = `sha256:${"c".repeat(64)}`;
    if (fault === "mutable") f.root.Values.inferenceRouter.image.tag = "latest";
    if (fault === "repository") f.root.Values.inferenceRouter.image.repository = "unreviewed.example/router";
    if (fault === "catalog") f.root.Values.inferenceBudget.catalogVersion = "replacement";
    if (fault === "funding") f.root.Values.sre.firstInstallFunding.tokens++;
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "status") f.registration.status.phase = "Retired";
    expect(() => render(f)).toThrow();
  });
  it.each(["values", "source", "intent", "funding", "binding", "retired", "controller", "runtime", "status", "owner"])("rejects changed %s without replacing authority", fault => {
    const f = fixture();
    if (fault === "values") f.root.Values.sre.model = "changed";
    if (fault === "source") f.source.metadata.uid = "replacement";
    if (fault === "intent") f.intentMap.metadata.uid = "replacement";
    if (fault === "funding") f.registration.spec.firstInstallFunding.grant.limits.tokens++;
    if (fault === "binding") delete f.source.spec.inferenceBudgetRef;
    if (fault === "retired") f.registration.spec.enabled = false;
    if (fault === "controller") f.controller.metadata.uid = "replacement";
    if (fault === "runtime") f.runtime.metadata.uid = "replacement";
    if (fault === "status") f.registration.status.observedGeneration = 0;
    if (fault === "owner") f.intentMap.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
    expect(() => render(f)).toThrow();
  });
});
