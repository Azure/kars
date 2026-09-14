// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import {
  PRIVATE_PREFIX as P, stagePrivateActivation, validateQualifiedActivation,
  bundleDefinition, type Execute,
} from "./private-activation.js";
import { continuityFixture as setup, privateAuthoritySnapshot } from "./private-activation-fixtures.js";

const RESOURCE = "karscredentialgrants.kars.azure.com";
const HISTORY = `${P}root-retirement`;
const ROOT = HISTORY;
const SCOPE = HISTORY;

function legacy(f: ReturnType<typeof setup>): void {
  const namespace = f.namespace("core");
  namespace.metadata.annotations[ROOT] = JSON.parse(namespace.metadata.annotations[ROOT]).retirement;
  namespace.metadata.resourceVersion = String(Number(namespace.metadata.resourceVersion) + 1);
}

describe("completed private qualification continuity", () => {
  it("enrolls a second distinct workspace with the first grant, authority and shared epochs byte-for-byte intact", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const before = f.preserved();
    const first = structuredClone(f.grant().spec.privateActivation);
    f.calls.length = 0;
    const review = await f.document("second");
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
    await applyReviewedGrant(f.execute, review);
    expect(f.preserved()).toEqual(before);
    expect(f.grant("second").spec.privateActivation.namespaces.find((scope: any) => scope.namespace.name === "core").epoch)
      .toBe(first.namespaces.find((scope: any) => scope.namespace.name === "core").epoch);
    expect(f.grant("second").spec.privateActivation.namespaces.find((scope: any) => scope.namespace.name === "reader").epoch)
      .toBe(first.namespaces.find((scope: any) => scope.namespace.name === "reader").epoch);
    expect(f.authority.size).toBe(2);
    expect(f.calls.filter(args => args[0] === "patch").every(args => args[1] === "namespace" && args[2] === "second")).toBe(true);
    await validateQualifiedActivation(f.execute, first);
    await validateQualifiedActivation(f.execute, f.grant("second").spec.privateActivation);
  });

  it("anchors both lifecycle receipts in the existing operator-only field, not projector-writable annotations", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    await applyReviewedGrant(f.execute, await f.document("second"));
    expect(JSON.parse(f.namespace("core").metadata.annotations[HISTORY]).version).toBe(2);
    expect(JSON.parse(f.namespace("second").metadata.annotations[HISTORY]).version).toBe(3);
    const policy = (bundleDefinition().objects as any[]).find(object =>
      object.kind === "ValidatingAdmissionPolicy" && object.metadata.name === "kars-private-consumption-namespace");
    expect(policy.spec.validations.some((validation: any) =>
      validation.expression === "variables.manager || variables.a[?'kars.azure.com/private-root-retirement'].orValue('') == oldObject.metadata.?annotations.orValue({})[?'kars.azure.com/private-root-retirement'].orValue('')")).toBe(true);
    for (const name of ["core", "second"]) {
      expect(f.namespace(name).metadata.annotations[`${P}root-qualification`]).toBeUndefined();
      expect(f.namespace(name).metadata.annotations[`${P}scope-qualification`]).toBeUndefined();
    }
  });

  it("updates/revokes only the selected grant and supports re-enrollment with no grants left while namespace evidence remains", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    await applyReviewedGrant(f.execute, await f.document("second"));
    const other = structuredClone(f.grant("second"));
    const namespaces = ["work", "core", "reader", "second"].map(name => privateAuthoritySnapshot(f.namespace(name)));
    const review = await f.document();
    f.calls.length = 0;
    await applyReviewedGrant(f.execute, { ...review, spec: { ...review.spec, agentKeys: ["CUSTOM_API_KEY"] } });
    expect(f.grant("second")).toEqual(other);
    expect(f.calls.filter(args => args[0] === "patch").every(args => args[1] === RESOURCE && args.includes("work"))).toBe(true);
    expect(["work", "core", "reader", "second"].map(name => privateAuthoritySnapshot(f.namespace(name)))).toEqual(namespaces);
    for (const work of ["work", "second"]) {
      const prior = structuredClone(f.grant(work));
      await applyReviewedGrant(f.execute, { ...prior, spec: { ...prior.spec, writers: [] } });
    }
    expect(f.authority.size).toBe(0);
    for (const work of ["work", "second"]) f.objects.delete(f.key(RESOURCE, "workspace", work));
    f.calls.length = 0;
    await applyReviewedGrant(f.execute, await f.document());
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
    expect(["work", "core", "reader", "second"].map(name => privateAuthoritySnapshot(f.namespace(name)))).toEqual(namespaces);
  });

  it("migrates a completed v1 restoring record using its original stored grant, without rewriting retirement history", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    legacy(f);
    const original = f.namespace("core").metadata.annotations[HISTORY];
    const first = structuredClone(f.grant());
    const deployment = structuredClone(f.deployment);
    f.calls.length = 0;
    const review = await f.document("second");
    expect(JSON.parse(f.namespace("core").metadata.annotations[ROOT]).version).toBe(1);
    await applyReviewedGrant(f.execute, review);
    expect(JSON.parse(f.namespace("core").metadata.annotations[ROOT]).version).toBe(2);
    expect(JSON.parse(f.namespace("core").metadata.annotations[HISTORY]).retirement).toBe(original);
    expect(f.grant()).toEqual(first);
    expect(f.deployment).toEqual(deployment);
    expect(f.calls.some(args => args[0] === "get" && args[1] === RESOURCE && args.includes("--all-namespaces"))).toBe(true);
  });

  it("does not infer a legacy original namespace set when no original review/grant remains", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    legacy(f);
    f.objects.delete(f.key(RESOURCE, "workspace", "work"));
    f.calls.length = 0;
    await expect(f.document("second")).rejects.toThrow("original exact review or stored qualified grant");
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    await applyReviewedGrant(f.execute, await f.document());
    await applyReviewedGrant(f.execute, await f.document("second"));
  });

  it("does not migrate a legacy completion with epochs that differ from the original stored grant", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    legacy(f);
    f.namespace("reader").metadata.annotations[`${P}epoch`] = "b".repeat(64);
    f.calls.length = 0;
    await expect(f.document()).rejects.toThrow();
    await expect(f.document("second")).rejects.toThrow();
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it("cannot discard retirement history or extend a shared consumer review", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    f.objects.set(f.key("deployments.apps", "extra", "reader"), {
      kind: "Deployment", metadata: { name: "extra", uid: "extra", resourceVersion: "1" },
      spec: { template: { metadata: {}, spec: { containers: [] } } },
    });
    f.calls.length = 0;
    await expect(f.document("second", ["reader/Deployment/extra"])).rejects.toThrow();
    delete f.namespace("core").metadata.annotations[HISTORY];
    await expect(f.document()).rejects.toThrow("lacks its original retirement history");
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it.each(["root-uid", "root-namespace", "deployment", "template", "replicas", "profile", "bundle",
    "namespace", "namespace-epoch", "template-epoch", "parent", "retirement", "proof", "captured", "execution", "owner", "ready"])(
    "preserves authority without mutation on changed %s evidence", async fault => {
      const f = setup();
      await applyReviewedGrant(f.execute, await f.document());
      if (fault === "root-uid") f.objects.get(f.key("serviceaccount", "kars-controller", "core")).metadata.uid = "replaced";
      if (fault === "root-namespace") f.namespace("core").metadata.uid = "replaced";
      if (fault === "deployment") f.deployment.metadata.uid = "replaced";
      if (fault === "template") f.deployment.spec.template.spec.containers[0]!.image = "replaced";
      if (fault === "replicas") f.deployment.spec.replicas = 0;
      if (fault === "bundle") f.objects.get(f.key("validatingadmissionpolicy", "kars-private-consumption")).metadata.resourceVersion = "2";
      if (fault === "namespace") f.namespace("reader").metadata.uid = "replaced";
      if (fault === "namespace-epoch") f.namespace("reader").metadata.annotations[`${P}epoch`] = "b".repeat(64);
      if (fault === "template-epoch") (f.deployment.spec.template.metadata as any).annotations[`${P}epoch`] = "b".repeat(64);
      if (fault === "parent") f.namespace("reader").metadata.annotations[`${P}parent-unreviewed`] = f.namespace("reader").metadata.annotations[`${P}epoch`];
      if (fault === "retirement") {
        const saved = JSON.parse(f.namespace("core").metadata.annotations[HISTORY]);
        saved.retirement = JSON.stringify({ ...JSON.parse(saved.retirement), attempt: "b".repeat(64) });
        f.namespace("core").metadata.annotations[HISTORY] = JSON.stringify(saved);
      }
      if (fault === "proof") f.namespace("core").metadata.annotations[ROOT] = "{}";
      if (fault === "execution") f.pods.get("core")![0].spec.containers[0].command = ["unreviewed"];
      if (fault === "owner") f.objects.get(f.key("replicasets.apps", "root-rs", "core")).metadata.uid = "replacement";
      if (fault === "ready") f.deployment.status.availableReplicas = 0;
      if (fault === "captured") f.pods.get("core")!.push({
        metadata: { name: "old-root", uid: "old-root", resourceVersion: "1" },
        spec: { automountServiceAccountToken: false, containers: [] },
      });
      const before = f.preserved();
      f.calls.length = 0;
      await expect(f.preview("second", [], f.execute, fault === "profile" ? "service-accounts" : "kcm-certificate")).rejects.toThrow();
      expect(f.preserved()).toEqual(before);
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
    });

  it("rejects a changed service-account controller profile and a tampered reviewed epoch before any mutation", async () => {
    const f = setup();
    const first = await f.document();
    first.spec.privateActivation = await f.preview("work", [], f.execute, "service-accounts");
    await applyReviewedGrant(f.execute, first);
    const review = await f.preview("second", [], f.execute, "service-accounts");
    review.namespaces.find(scope => scope.namespace.name === "reader")!.epoch = "c".repeat(64);
    f.calls.length = 0;
    await expect(stagePrivateActivation(f.execute, review)).rejects.toThrow();
    f.objects.get(f.key("serviceaccount", "replicaset-controller", "kube-system")).metadata.uid = "changed";
    await expect(f.preview("second", [], f.execute, "service-accounts")).rejects.toThrow();
    expect(f.calls.every(args => ["get", "auth"].includes(args[0]!))).toBe(true);
  });

  it.each(["core", "reader", "second"])("preserves an unreviewed consuming Pod in %s without restarting shared consumers", async scope => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const before = structuredClone(f.deployment);
    f.pods.set(scope, [{
      kind: "Pod", metadata: { name: "foreign", uid: "foreign", resourceVersion: "1",
        deletionTimestamp: "2026-01-01T00:00:00Z",
        annotations: { [`${P}epoch`]: f.namespace(scope).metadata.annotations[`${P}epoch`] ?? "a".repeat(64) } },
      spec: { containers: [{ name: "private", image: "fixture" }],
        volumes: [{ name: "private", secret: { secretName: "router-services-observer-identity" } }] },
    }]);
    f.calls.length = 0;
    await expect(f.document("second")).rejects.toThrow();
    expect(f.deployment).toEqual(before);
    expect(f.pods.get(scope)?.[0].metadata.uid).toBe("foreign");
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
  });

  it("rejects a namespace with only unproven Pending/Qualified markers rather than inventing an epoch", async () => {
    for (const state of ["Pending", "Qualified"]) {
      const f = setup();
      await applyReviewedGrant(f.execute, await f.document());
      f.namespace("second").metadata.annotations = { [`${P}enabled`]: "true", [`${P}state`]: state };
      f.calls.length = 0;
      await expect(f.document("second")).rejects.toThrow("unproven private lifecycle");
      expect(f.calls.every(args => args[0] === "get")).toBe(true);
    }
  });

  it("resumes interrupted additional-scope Pending staging without rotating the root or earlier epochs", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const before = f.preserved();
    const interrupted: Execute = async (args, input) => {
      if (args[0] === "patch" && args[2] === "second"
        && JSON.parse(args[args.indexOf("-p") + 1]!).metadata.annotations[`${P}epoch`]) throw new Error("namespace conflict");
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(interrupted, await f.document("second"))).rejects.toThrow("namespace conflict");
    expect(JSON.parse(f.namespace("second").metadata.annotations[SCOPE]).phase).toBe("Pending");
    expect(f.namespace("second").metadata.annotations[`${P}epoch`]).toBeUndefined();
    await applyReviewedGrant(f.execute, await f.document("second"));
    expect(f.preserved()).toEqual(before);
  });

  it("fences a namespace resourceVersion change between review and the new scope's first patch", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const before = f.preserved();
    const review = await f.document("second");
    const raced: Execute = async (args, input) => {
      if (args[0] === "patch" && args[1] === "namespace" && args[2] === "second") {
        f.namespace("second").metadata.resourceVersion = "2";
      }
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(raced, review)).rejects.toThrow();
    expect(f.namespace("second").metadata.annotations).toEqual({});
    expect(f.preserved()).toEqual(before);
  });

  it("keeps the exact v1 recovery JSON and issued epochs when the completion seal CAS fails", async () => {
    const f = setup();
    const interrupted: Execute = async (args, input) => {
      if (args[0] === "patch" && args[2] === "core") {
        const raw = JSON.parse(args[args.indexOf("-p") + 1]!).metadata.annotations?.[HISTORY];
        if (raw && JSON.parse(raw).version === 2) throw new Error("completion CAS conflict");
      }
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(interrupted, await f.document())).rejects.toThrow("completion CAS conflict");
    const history = f.namespace("core").metadata.annotations[HISTORY];
    const root = structuredClone(f.deployment);
    const epochs = ["work", "reader", "core"].map(name => f.namespace(name).metadata.annotations[`${P}epoch`]);
    expect(JSON.parse(history).version).toBe(1);
    expect(JSON.parse(history).phase).toBe("restoring");
    await applyReviewedGrant(f.execute, await f.document());
    expect(JSON.parse(f.namespace("core").metadata.annotations[HISTORY]).retirement).toBe(history);
    expect(["work", "reader", "core"].map(name => f.namespace(name).metadata.annotations[`${P}epoch`])).toEqual(epochs);
    expect(f.deployment).toEqual(root);
  });

  it("preserves a failed first pause and refuses to widen its original recovery scope", async () => {
    const f = setup();
    const interrupted: Execute = async (args, input) => {
      if (args[0] === "patch" && args[2] === "kars-controller"
        && JSON.parse(args[args.indexOf("-p") + 1]!).spec?.replicas === 0) throw new Error("pause conflict");
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(interrupted, await f.document())).rejects.toThrow("pause conflict");
    const history = f.namespace("core").metadata.annotations[HISTORY];
    expect(JSON.parse(history).phase).toBe("pausing");
    expect(JSON.parse(history).captured["core-uid"]).toEqual(["old-root"]);
    f.calls.length = 0;
    await expect(f.document("second")).rejects.toThrow("retirement");
    expect(f.namespace("core").metadata.annotations[HISTORY]).toBe(history);
    expect(f.calls.every(args => args[0] === "get")).toBe(true);
    await applyReviewedGrant(f.execute, await f.document());
  });

  it("resumes a failed new consumer stamp with the already-issued scope epoch, not a second retirement", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const consumer = { kind: "Deployment", metadata: { name: "router", uid: "router", resourceVersion: "1", namespace: "second" },
      spec: { replicas: 0, template: { metadata: {}, spec: { containers: [{ name: "router", image: "fixture" }],
        volumes: [{ name: "identity", secret: { secretName: "router-services-observer-identity" } }] } } } };
    f.objects.set(f.key("deployments.apps", "router", "second"), consumer);
    const reviewed = ["second/Deployment/router"];
    const before = f.preserved();
    const interrupted: Execute = async (args, input) => {
      if (args[0] === "patch" && args[2] === "router") throw new Error("template conflict");
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(interrupted, await f.document("second", reviewed))).rejects.toThrow("template conflict");
    const epoch = f.namespace("second").metadata.annotations[`${P}epoch`];
    expect(JSON.parse(f.namespace("second").metadata.annotations[SCOPE]).phase).toBe("Stamping");
    await applyReviewedGrant(f.execute, await f.document("second", reviewed));
    expect(f.namespace("second").metadata.annotations[`${P}epoch`]).toBe(epoch);
    expect(JSON.parse(f.namespace("second").metadata.annotations[SCOPE]).phase).toBe("Qualified");
    expect(f.preserved()).toEqual(before);
  });

  it("resumes the original interrupted restore, but never treats it as completed shared reuse", async () => {
    const f = setup();
    const interrupted: Execute = async (args, input) => {
      if (args[0] === "patch" && args[2] === "kars-controller"
        && JSON.parse(args[args.indexOf("-p") + 1]!).spec?.replicas === 1) throw new Error("restore conflict");
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(interrupted, await f.document())).rejects.toThrow("restore conflict");
    const epochs = ["work", "reader", "core"].map(name => f.namespace(name).metadata.annotations[`${P}epoch`]);
    const history = f.namespace("core").metadata.annotations[HISTORY];
    expect(f.deployment.spec.replicas).toBe(0);
    expect(JSON.parse(f.namespace("core").metadata.annotations[ROOT]).version).toBe(1);
    await expect(f.document("second")).rejects.toThrow("original exact review");
    await applyReviewedGrant(f.execute, await f.document());
    expect(["work", "reader", "core"].map(name => f.namespace(name).metadata.annotations[`${P}epoch`])).toEqual(epochs);
    expect(JSON.parse(f.namespace("core").metadata.annotations[HISTORY]).retirement).toBe(history);
  });

  it("fences concurrent same-scope previews and permits independent new scopes without a shared-root write", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const before = f.preserved();
    const first = await f.document("second");
    const stale = await f.document("second");
    const third = await f.document("third");
    f.calls.length = 0;
    await Promise.all([applyReviewedGrant(f.execute, first), applyReviewedGrant(f.execute, third)]);
    const count = f.calls.filter(args => args[0] === "patch").length;
    await expect(applyReviewedGrant(f.execute, stale)).rejects.toThrow("namespace changed");
    expect(f.calls.filter(args => args[0] === "patch").length).toBe(count);
    expect(f.preserved()).toEqual(before);
  });
});
