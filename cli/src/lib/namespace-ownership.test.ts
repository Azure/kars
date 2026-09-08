// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import type { Execute } from "./deployment-target.js";
import {
  CLAIM, adoptNamespace, inspectNamespaceOwnership, legacyNamespaceProof,
  namespaceClaimed, namespacePrestaged, prepareCredentialNamespace, type OwnershipObject,
} from "./namespace-ownership.js";

function fixture(): { sandbox: OwnershipObject; namespace: OwnershipObject; deployment: OwnershipObject } {
  return JSON.parse(readFileSync(new URL("../../../tests/compat/fixtures/namespace-legacy.json", import.meta.url), "utf8"));
}

function cluster() {
  const f = fixture();
  const state = {
    sandboxes: [f.sandbox], namespace: f.namespace as OwnershipObject | undefined,
    deployment: f.deployment as OwnershipObject | undefined, readError: false, patchError: false,
  };
  const run = vi.fn(async (command: string, args: readonly string[], options?: { input?: string }) => {
    expect(command).toBe("kubectl");
    if (args[0] === "get") {
      if (state.readError) throw new Error("Forbidden (403)");
      expect(args).toContain("--show-managed-fields=true");
      const value = args[1] === "karssandboxes" ? { items: state.sandboxes }
        : args[1] === "namespace" ? state.namespace : state.deployment;
      return { stdout: value ? JSON.stringify(value) : "" };
    }
    if (args[0] === "patch") {
      if (state.patchError) throw new Error("Conflict (409)");
      expect(args.slice(0, 4)).toEqual(["patch", "namespace", "kars-demo", "--type=merge"]);
      const patch = JSON.parse(args[5]);
      expect(patch.metadata.uid).toBe(state.namespace?.metadata.uid);
      expect(patch.metadata.resourceVersion).toBe(state.namespace?.metadata.resourceVersion);
      state.namespace!.metadata.annotations = {
        ...state.namespace!.metadata.annotations, ...patch.metadata.annotations,
      };
      return { stdout: "" };
    }
    if (args[0] === "create") {
      expect(state.namespace).toBeUndefined();
      const created = JSON.parse(options!.input!) as OwnershipObject;
      created.metadata.uid = "reserved-namespace";
      created.metadata.resourceVersion = "101";
      created.metadata.creationTimestamp = "2026-09-01T09:59:00Z";
      state.namespace = created;
      return { stdout: JSON.stringify(created) };
    }
    throw new Error(`Unexpected operation ${args.join(" ")}`);
  });
  return { state, run, execute: run as unknown as Execute };
}

describe("namespace claim v1 and shared legacy evidence", () => {
  it("accepts genuine legacy deployments even when the CLI created the namespace before the CR", () => {
    const f = fixture();
    expect(legacyNamespaceProof(f.namespace, f.deployment, f.sandbox)).toBe(true);
    expect(namespaceClaimed(f.namespace, f.sandbox)).toBe(false);
  });

  it("does not infer ownership from labels alone", () => {
    const f = fixture();
    f.namespace.metadata.managedFields = [];
    expect(legacyNamespaceProof(f.namespace, f.deployment, f.sandbox)).toBe(false);
  });

  it("rejects deployment parent mismatch and newer CR incarnation", () => {
    const f = fixture();
    f.deployment.metadata.labels!["kars.azure.com/parent-namespace"] = "other";
    expect(legacyNamespaceProof(f.namespace, f.deployment, f.sandbox)).toBe(false);
    delete f.deployment.metadata.labels!["kars.azure.com/parent-namespace"];
    expect(legacyNamespaceProof(f.namespace, f.deployment, f.sandbox)).toBe(true);
    f.sandbox.metadata.uid = "new-incarnation";
    f.sandbox.metadata.creationTimestamp = "2026-09-01T12:00:00Z";
    expect(legacyNamespaceProof(f.namespace, f.deployment, f.sandbox)).toBe(false);
    f.sandbox.metadata.creationTimestamp = f.deployment.metadata.creationTimestamp;
    expect(legacyNamespaceProof(f.namespace, f.deployment, f.sandbox)).toBe(false);
  });

  it.each([
    "status", "namespace-manager", "deployment-manager", "finalizer", "deployment-timestamp", "selector", "pod-label",
  ])("requires every piece of legacy evidence: %s", field => {
    const f = fixture();
    if (field === "status") delete f.sandbox.status;
    if (field === "namespace-manager") f.namespace.metadata.managedFields![0].manager = "kubectl";
    if (field === "deployment-manager") f.deployment.metadata.managedFields![0].manager = "kubectl";
    if (field === "finalizer") f.sandbox.metadata.finalizers = [];
    if (field === "deployment-timestamp") delete f.deployment.metadata.creationTimestamp;
    if (field === "selector") f.deployment.spec!.selector!.matchLabels = {};
    if (field === "pod-label") f.deployment.spec!.template!.metadata!.labels = {};
    expect(legacyNamespaceProof(f.namespace, f.deployment, f.sandbox)).toBe(false);
  });

  it.each([CLAIM.uid, CLAIM.namespace, CLAIM.name, CLAIM.version])("rejects a foreign/partial claim: %s", key => {
    const f = fixture();
    f.namespace.metadata.annotations![key] = "foreign";
    expect(() => namespaceClaimed(f.namespace, f.sandbox)).toThrow("NamespaceOwnershipConflict");
  });

  it("rejects arbitrary ownerReferences and replacement namespace UIDs", () => {
    const f = fixture();
    f.namespace.metadata.ownerReferences = [{ uid: "customer-owner" }];
    expect(() => namespaceClaimed(f.namespace, f.sandbox)).toThrow("ownerReferences");
    f.namespace.metadata.ownerReferences = [];
    f.sandbox.metadata.annotations = { [CLAIM.namespaceUid]: "old-namespace" };
    expect(() => namespaceClaimed(f.namespace, f.sandbox)).toThrow("replaced");
  });
});

describe("read-only upgrade preflight", () => {
  it("reports proven legacy adoption without mutating workloads, namespaces, or data", async () => {
    const { state, execute, run } = cluster();
    const original = structuredClone(state);
    await expect(inspectNamespaceOwnership(execute)).resolves.toEqual([
      "workspace-a/demo: unambiguous legacy deployment (metadata-only adoption)",
    ]);
    expect(state).toEqual(original);
    expect(run.mock.calls.every(([, args]) => args[0] === "get")).toBe(true);
    expect(run.mock.calls.some(([, args]) => args.includes("secret"))).toBe(false);
  });

  it("blocks ambiguous same-name Sandboxes in different workspaces", async () => {
    const { state, execute, run } = cluster();
    const other = structuredClone(state.sandboxes[0]);
    other.metadata.namespace = "workspace-b";
    other.metadata.uid = "sandbox-b";
    state.sandboxes.push(other);
    await expect(inspectNamespaceOwnership(execute)).rejects.toThrow("multiple possible owners");
    expect(run.mock.calls.every(([, args]) => args[0] === "get")).toBe(true);
  });

  it("rejects namespaces without workloads or valid evidence and surfaces read errors", async () => {
    const { state, execute } = cluster();
    state.deployment = undefined;
    await expect(inspectNamespaceOwnership(execute)).rejects.toThrow("explicit adoption required");
    state.readError = true;
    await expect(inspectNamespaceOwnership(execute)).rejects.toThrow("403");
  });

  it("recognizes empty clusters and new namespaces but not missing bound namespaces", async () => {
    const { state, execute } = cluster();
    state.namespace = undefined;
    await expect(inspectNamespaceOwnership(execute)).resolves.toEqual([
      "workspace-a/demo: new namespace (atomic create)",
    ]);
    state.sandboxes[0].metadata.annotations = { [CLAIM.namespaceUid]: "old" };
    await expect(inspectNamespaceOwnership(execute)).rejects.toThrow("missing");
    state.sandboxes = [];
    await expect(inspectNamespaceOwnership(execute)).resolves.toEqual([]);
  });
});

describe("explicit administrator adoption", () => {
  it("uses reviewed UIDs and CAS, changing only claim annotations", async () => {
    const { state, execute } = cluster();
    const original = structuredClone(state);
    await adoptNamespace(execute, "demo", "workspace-a", "sandbox-a", "namespace-a");
    expect(namespaceClaimed(state.namespace!, state.sandboxes[0])).toBe(true);
    expect(state.namespace!.metadata.labels).toEqual(original.namespace!.metadata.labels);
    expect(state.namespace!.metadata.annotations!["customer-annotation"]).toBe("keep");
    expect(state.deployment).toEqual(original.deployment);
    expect(state.sandboxes).toEqual(original.sandboxes);
  });

  it("rejects stale reviewed UIDs, foreign claims, and namespace write conflicts", async () => {
    const { state, execute } = cluster();
    await expect(adoptNamespace(execute, "demo", "workspace-a", "stale", "namespace-a")).rejects.toThrow("UID");
    await expect(adoptNamespace(execute, "demo", "workspace-a", "sandbox-a", "stale")).rejects.toThrow("UID");
    state.namespace!.metadata.annotations![CLAIM.uid] = "foreign";
    await expect(adoptNamespace(execute, "demo", "workspace-a", "sandbox-a", "namespace-a")).rejects.toThrow("different");
    delete state.namespace!.metadata.annotations![CLAIM.uid];
    state.patchError = true;
    await expect(adoptNamespace(execute, "demo", "workspace-a", "sandbox-a", "namespace-a")).rejects.toThrow("409");
  });
});

describe("add credential namespace prestaging", () => {
  it("rejects path-shaped inputs before querying or creating namespaces", async () => {
    const { execute, run } = cluster();
    await expect(prepareCredentialNamespace(execute, "../other", "workspace-a")).rejects.toThrow("DNS label");
    await expect(adoptNamespace(execute, "demo", "../other", "sandbox-a", "namespace-a")).rejects.toThrow("DNS label");
    expect(run).not.toHaveBeenCalled();
  });

  it("creates a reservation before the CR and requires its UID backlink for binding", async () => {
    const { state, execute, run } = cluster();
    state.sandboxes = [];
    state.namespace = undefined;
    expect(await prepareCredentialNamespace(execute, "demo", "workspace-a")).toBe("reserved-namespace");
    const sandbox = fixture().sandbox;
    expect(namespacePrestaged(state.namespace!, sandbox)).toBe(false);
    sandbox.metadata.annotations = { [CLAIM.namespaceUid]: "reserved-namespace" };
    expect(namespacePrestaged(state.namespace!, sandbox)).toBe(true);
    expect(await prepareCredentialNamespace(execute, "demo", "workspace-a")).toBe("reserved-namespace");
    state.namespace!.metadata.annotations![CLAIM.uid] = "previous-owner";
    expect(namespacePrestaged(state.namespace!, sandbox)).toBe(false);
    expect(run.mock.calls.filter(([, args]) => args[0] === "create")).toHaveLength(1);
  });

  it("retains the existing legacy credential namespace when its ownership is proven", async () => {
    const { state, execute, run } = cluster();
    const original = structuredClone(state);
    expect(await prepareCredentialNamespace(execute, "demo", "workspace-a")).toBe("namespace-a");
    expect(state).toEqual(original);
    expect(run.mock.calls.every(([, args]) => args[0] === "get")).toBe(true);
  });

  it("never claims an arbitrary or foreign-workspace namespace", async () => {
    const { state, execute } = cluster();
    await expect(prepareCredentialNamespace(execute, "demo", "workspace-b")).rejects.toThrow("another workspace");
    state.sandboxes = [];
    await expect(prepareCredentialNamespace(execute, "demo", "workspace-a")).rejects.toThrow("explicit adoption");
  });

  it("does not swallow a create409 race or namespace read failure", async () => {
    const { state, execute, run } = cluster();
    state.sandboxes = [];
    state.namespace = undefined;
    run.mockImplementation(async (_command, args) => {
      if (args[0] === "get") return { stdout: args[1] === "karssandboxes" ? '{"items":[]}' : "" };
      throw new Error("AlreadyExists (409)");
    });
    await expect(prepareCredentialNamespace(execute, "demo", "workspace-a")).rejects.toThrow("409");
    expect(run.mock.calls.some(([, args]) => args[0] === "patch" || args[0] === "delete")).toBe(false);
  });
});
