// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { rootCertificates } from "node:tls";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import { PRIVATE_PREFIX as P, previewPrivateActivation, stagePrivateActivation, validateQualifiedActivation, type Execute } from "./private-activation.js";
import { captureGuardRetirement, refreshGuardRetirement } from "./private-activation-guard-retirement.js";
import { continuityFixture as setup, rootPod } from "./private-activation-fixtures.js";

const RESOURCE = "karscredentialgrants.kars.azure.com";
const key = (uid: string) => `kars.azure.com/credential-reader-${uid}`;
const isQuiesce = (args: string[]) => args[0] === "patch" && args[1] === RESOURCE
  && JSON.parse(args[args.indexOf("-p") + 1]!).spec.writers.length === 0;
const update = (document: any) => ({ ...document, spec: { ...document.spec, agentKeys: ["CUSTOM_API_KEY"] } });

function expectedRemoval(namespace: any, guard: string): any {
  const expected = structuredClone(namespace);
  delete expected.metadata.labels[guard];
  delete expected.metadata.annotations[guard];
  expected.metadata.finalizers = expected.metadata.finalizers.filter((value: string) => value !== guard);
  for (const field of ["labels", "annotations", "finalizers"]) {
    if (!Object.keys(expected.metadata[field]).length) delete expected.metadata[field];
  }
  return expected;
}

describe("selected writer guard retirement review", () => {
  it.each([false, true])("updates ordinary keys after actual guard/RV removal with shared writer=%s", async shared => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    if (shared) await applyReviewedGrant(f.execute, await f.document("second"));
    const guard = key(f.grant().metadata.uid);
    const namespace = structuredClone(f.namespace("reader"));
    const root = structuredClone(f.namespace("core"));
    const deployment = structuredClone(f.deployment);
    const previous = structuredClone(f.grant().spec.privateActivation);
    const other = shared ? structuredClone(f.grant("second")) : undefined;
    const otherAuthority = shared ? structuredClone(f.authority.get(other.metadata.uid)) : undefined;
    const document = update(await f.document());
    const reviewedDocument = structuredClone(document);
    let retired: any;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (isQuiesce(args)) retired = structuredClone(f.namespace("reader"));
      return result;
    };
    f.calls.length = 0;
    await applyReviewedGrant(execute, document);
    const expected = expectedRemoval(namespace, guard);
    expected.metadata.resourceVersion = retired.metadata.resourceVersion;
    expect(retired).toEqual(expected);
    expect(retired.metadata.resourceVersion).not.toBe(namespace.metadata.resourceVersion);
    expect(retired.metadata.uid).toBe(namespace.metadata.uid);
    expect(retired.spec.finalizers).toEqual(namespace.spec.finalizers);
    const stored = f.grant().spec.privateActivation;
    expect(stored.namespaces.find((scope: any) => scope.namespace.name === "reader").namespace.resourceVersion)
      .toBe(retired.metadata.resourceVersion);
    expect(stored.namespaces.map((scope: any) => scope.epoch)).toEqual(previous.namespaces.map((scope: any) => scope.epoch));
    expect(f.grant().spec.agentKeys).toEqual(["CUSTOM_API_KEY"]);
    expect(f.grant().spec.writers).toHaveLength(1);
    expect(f.namespace("core")).toEqual(root);
    expect(f.deployment).toEqual(deployment);
    expect(document).toEqual(reviewedDocument);
    expect(f.calls.filter(args => args[0] === "patch").every(args => args[1] === RESOURCE)).toBe(true);
    if (shared) {
      const otherKey = key(other.metadata.uid);
      expect(retired.metadata.labels[otherKey]).toBe(namespace.metadata.labels[otherKey]);
      expect(retired.metadata.annotations[otherKey]).toBe(namespace.metadata.annotations[otherKey]);
      expect(retired.metadata.finalizers).toContain(otherKey);
      expect(f.grant("second")).toEqual(other);
      expect(f.authority.get(other.metadata.uid)).toEqual(otherAuthority);
      await validateQualifiedActivation(f.execute, other.spec.privateActivation);
    }
    await validateQualifiedActivation(f.execute, stored);
  });

  it.each(["label", "annotation", "finalizer", "native-finalizer", "uid", "receipt", "epoch",
    "other-label", "other-annotation", "other-finalizer", "partial-guard", "rv-only", "non-writer"])(
    "rejects unrelated %s drift without overwriting it or another grant", async fault => {
      const f = setup();
      await applyReviewedGrant(f.execute, await f.document());
      await applyReviewedGrant(f.execute, await f.document("second"));
      const other = structuredClone(f.grant("second"));
      const otherAuthority = structuredClone(f.authority.get(other.metadata.uid));
      const otherKey = key(other.metadata.uid);
      const ownKey = key(f.grant().metadata.uid);
      const baseline = structuredClone(f.namespace("reader"));
      const root = structuredClone(f.namespace("core"));
      let changed: any;
      const execute: Execute = async (args, input) => {
        const result = await f.execute(args, input);
        if (isQuiesce(args)) {
          const ns = f.namespace(fault === "non-writer" ? "work" : "reader");
          const meta = ns.metadata;
          if (fault === "label") meta.labels["example.test/external"] = "changed";
          if (fault === "annotation" || fault === "non-writer") meta.annotations["example.test/external"] = "changed";
          if (fault === "finalizer") meta.finalizers.push("example.test/external");
          if (fault === "native-finalizer") ns.spec.finalizers = [];
          if (fault === "uid") meta.uid = "replacement";
          if (fault === "receipt") meta.annotations[`${P}root-retirement`] = '{"external":true}';
          if (fault === "epoch") meta.annotations[`${P}epoch`] = "b".repeat(64);
          if (fault === "other-label") delete meta.labels[otherKey];
          if (fault === "other-annotation") delete meta.annotations[otherKey];
          if (fault === "other-finalizer") meta.finalizers = meta.finalizers.filter((value: string) => value !== otherKey);
          if (fault === "partial-guard") meta.annotations[ownKey] = "controller-sa";
          if (fault === "rv-only") ns.metadata = structuredClone(baseline.metadata);
          ns.metadata.resourceVersion = String(Number(ns.metadata.resourceVersion) + 1);
          changed = structuredClone(ns);
        }
        return result;
      };
      await expect(applyReviewedGrant(execute, update(await f.document()))).rejects.toThrow("expected writer-guard retirement");
      expect(f.namespace(fault === "non-writer" ? "work" : "reader")).toEqual(changed);
      expect(f.namespace("core")).toEqual(root);
      expect(f.grant().spec.writers).toEqual([]);
      expect(f.grant().spec.agentKeys).toBeUndefined();
      expect(f.grant("second")).toEqual(other);
      expect(f.authority.get(other.metadata.uid)).toEqual(otherAuthority);
    });

  it.each(["root", "template", "profile", "budget"])("revalidates the original %s input after the allowed guard delta", async fault => {
    const f = setup();
    const document = await f.document();
    document.spec.privateActivation = await f.preview("work", [], f.execute, "service-accounts");
    await applyReviewedGrant(f.execute, document);
    const current = structuredClone(f.grant());
    current.spec.privateActivation = await f.preview("work", [], f.execute, "service-accounts");
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (isQuiesce(args)) {
        if (fault === "root") f.objects.get(f.key("serviceaccount", "kars-controller", "core")).metadata.uid = "replacement";
        if (fault === "template") f.deployment.spec.template.spec.containers[0]!.image = "replacement";
        if (fault === "profile") f.objects.get(f.key("serviceaccount", "replicaset-controller", "kube-system")).metadata.uid = "replacement";
        if (fault === "budget") (f.deployment.spec.template.spec.containers[0] as any).env =
          [{ name: "KARS_INFERENCE_BUDGET_ENABLED", value: "true" }];
      }
      return result;
    };
    await expect(applyReviewedGrant(execute, update(current))).rejects.toThrow();
    expect(f.grant().spec.writers).toEqual([]);
    expect(f.grant().spec.agentKeys).toBeUndefined();
  });

  it("rejects a changed budget Secret version after the expected namespace guard removal", async () => {
    const f = setup();
    (f.deployment.spec.template.spec.containers[0] as any).env = [
      { name: "KARS_INFERENCE_BUDGET_ENABLED", value: "true" },
      { name: "KARS_INFERENCE_BUDGET_TLS_SECRET", value: "budget-tls" },
      { name: "KARS_NAMESPACE", value: "core" },
    ];
    f.pods.set("core", [rootPod(f)]);
    const secret = { type: "kubernetes.io/tls",
      metadata: { name: "budget-tls", namespace: "core", uid: "budget-tls-uid", resourceVersion: "1",
        annotations: { "kars.azure.com/inference-budget-tls": "v1" } },
      data: { "tls.crt": Buffer.from(rootCertificates[0]!).toString("base64") } };
    f.objects.set(f.key("secret", "budget-tls", "core"), secret);
    await expect(applyReviewedGrant(f.execute, await f.document())).rejects.toThrow("operator rotation");
    secret.metadata.resourceVersion = "2";
    secret.data["tls.crt"] = Buffer.from(rootCertificates[1]!).toString("base64");
    await applyReviewedGrant(f.execute, await f.document());
    const review = update(await f.document());
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (isQuiesce(args)) secret.metadata.resourceVersion = "3";
      return result;
    };
    await expect(applyReviewedGrant(execute, review)).rejects.toThrow("budget TLS");
    expect(secret.metadata.resourceVersion).toBe("3");
    expect(f.grant().spec.writers).toEqual([]);
    expect(f.grant().spec.agentKeys).toBeUndefined();
  });

  it("does not adopt a namespace version that changed before the pre-retirement snapshot", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const document = update(await f.document());
    const before = structuredClone(f.grant());
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[0] === "get" && args[1] === RESOURCE && args.includes("--ignore-not-found")) {
        f.namespace("reader").metadata.annotations.external = "changed";
        f.namespace("reader").metadata.resourceVersion += "1";
      }
      return result;
    };
    await expect(applyReviewedGrant(execute, document)).rejects.toThrow("expected writer-guard retirement");
    expect(f.grant()).toEqual(before);
  });

  it.each(["controller", "namespace", "finalizer", "hold"])("requires the actual baseline guard %s convention before quiescing", async fault => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const prior = structuredClone(f.grant());
    const guard = key(prior.metadata.uid);
    const ns = f.namespace("reader");
    if (fault === "controller") ns.metadata.annotations[guard] = "different-controller";
    if (fault === "namespace") ns.metadata.labels[guard] = "different-namespace";
    if (fault === "finalizer") ns.metadata.finalizers = [];
    if (fault === "hold") ns.spec.finalizers = [];
    ns.metadata.resourceVersion += "1";
    const review = update(await f.document());
    f.calls.length = 0;
    await expect(applyReviewedGrant(f.execute, review)).rejects.toThrow("expected writer-guard retirement");
    expect(f.calls.some(args => args[0] === "patch")).toBe(false);
    expect(f.grant()).toEqual(prior);
  });

  it("does not relax the refreshed version fence for a later namespace mutation", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const document = await f.document();
    const prior = structuredClone(f.grant());
    const snapshot = await captureGuardRetirement(f.execute, document.spec.privateActivation, prior);
    await f.execute(["patch", RESOURCE, "workspace", "-n", "work", "--type=merge", "-p", JSON.stringify({
      metadata: { uid: prior.metadata.uid, resourceVersion: prior.metadata.resourceVersion }, spec: { ...prior.spec, writers: [] },
    })]);
    expect(f.authority.has(prior.metadata.uid)).toBe(false);
    await expect(stagePrivateActivation(f.execute, document.spec.privateActivation)).rejects.toThrow("Reviewed private namespace changed");
    const refreshed = await refreshGuardRetirement(f.execute, snapshot);
    expect(refreshed).toBeDefined();
    f.namespace("reader").metadata.annotations.external = "changed-after-refresh";
    f.namespace("reader").metadata.resourceVersion += "1";
    await expect(stagePrivateActivation(f.execute, refreshed!)).rejects.toThrow("Reviewed private namespace changed");
  });

  it("accounts for the retired writer namespace even when it is absent from the new writer review", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    await applyReviewedGrant(f.execute, await f.document("second"));
    f.objects.set(f.key("namespace", "new-reader"), {
      kind: "Namespace", metadata: { name: "new-reader", uid: "new-reader-uid", resourceVersion: "1", annotations: {} },
      spec: { finalizers: ["kubernetes"] },
    });
    f.objects.set(f.key("serviceaccount", "writer", "new-reader"), {
      metadata: { name: "writer", namespace: "new-reader", uid: "new-writer", resourceVersion: "1" },
    });
    const prior = structuredClone(f.grant());
    const other = structuredClone(f.grant("second"));
    const root = structuredClone(f.namespace("core"));
    const document = { ...prior, spec: { ...prior.spec, writers: [{ namespace: "new-reader", name: "writer", uid: "new-writer" }],
      privateActivation: await previewPrivateActivation(f.execute, "work", [{ namespace: "new-reader" }], [], "core", "kcm-certificate", []) } };
    expect(document.spec.privateActivation.namespaces.some((scope: any) => scope.namespace.name === "reader")).toBe(false);
    await applyReviewedGrant(f.execute, document);
    expect(f.namespace("reader").metadata.labels[key(prior.metadata.uid)]).toBeUndefined();
    expect(f.namespace("reader").metadata.labels[key(other.metadata.uid)]).toBe("reader-uid");
    expect(f.namespace("new-reader").metadata.labels[key(prior.metadata.uid)]).toBe("new-reader-uid");
    expect(f.grant("second")).toEqual(other);
    expect(f.namespace("core")).toEqual(root);
    await validateQualifiedActivation(f.execute, other.spec.privateActivation);
  });

  it("waits for delayed guard removal even when the writer/role retirement barrier already reports completion", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const namespace = structuredClone(f.namespace("reader"));
    let retired: any;
    let delayed = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (isQuiesce(args)) {
        retired = structuredClone(f.namespace("reader"));
        f.objects.set(f.key("namespace", "reader"), structuredClone(namespace));
        delayed = true;
      } else if (delayed && args[0] === "get" && args[1] === "namespace" && args[2] === "reader") {
        f.objects.set(f.key("namespace", "reader"), retired);
        delayed = false;
      }
      return result;
    };
    await applyReviewedGrant(execute, update(await f.document()));
    expect(f.grant().spec.agentKeys).toEqual(["CUSTOM_API_KEY"]);
    expect(f.grant().spec.writers).toHaveLength(1);
  });

  it.each(["intent", "roles"])("retains the selected-grant %s retirement fence", async fault => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const uid = f.grant().metadata.uid;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (isQuiesce(args)) {
        if (fault === "intent") f.grant().spec.agentKeys = ["EXTERNAL_TOKEN"];
        else f.authority.set(uid, { metadata: { annotations: { "kars.azure.com/credential-grant-owner": uid } } });
      }
      return result;
    };
    const document = update(await f.document());
    const now = vi.spyOn(Date, "now").mockReturnValueOnce(0).mockReturnValue(120_001);
    try {
      await expect(applyReviewedGrant(execute, document)).rejects.toThrow(
        fault === "intent" ? "Grant changed while retiring" : "retirement is still pending",
      );
    } finally { now.mockRestore(); }
    expect(f.grant().spec.writers).toEqual([]);
    expect(f.grant().spec.agentKeys).toEqual(fault === "intent" ? ["EXTERNAL_TOKEN"] : undefined);
  });

  it("rechecks selected intent after refreshing guard versions, before any activation staging", async () => {
    const f = setup();
    await applyReviewedGrant(f.execute, await f.document());
    const root = structuredClone(f.namespace("core"));
    let inventoryRead = false;
    const execute: Execute = async (args, input) => {
      const result = await f.execute(args, input);
      if (args[1]?.startsWith("roles,")) inventoryRead = true;
      else if (inventoryRead && args[0] === "get" && args[1] === "namespace") {
        f.grant().spec.agentKeys = ["EXTERNAL_TOKEN"];
        f.grant().metadata.resourceVersion += "1";
        inventoryRead = false;
      }
      return result;
    };
    await expect(applyReviewedGrant(execute, update(await f.document()))).rejects.toThrow("Grant changed after retiring");
    expect(f.grant().spec.agentKeys).toEqual(["EXTERNAL_TOKEN"]);
    expect(f.grant().spec.writers).toEqual([]);
    expect(f.namespace("core")).toEqual(root);
  });
});
