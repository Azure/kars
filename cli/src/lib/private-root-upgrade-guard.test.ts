// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import {
  assertControllerMutationAllowed, assertRenderedControllersMutable, PrivateRootUpgradeBlocked, type RootGuardExecute,
} from "./private-root-upgrade-guard.js";
import { restartController, restartSandboxes } from "./deployment-rollout.js";
import { rolloutRestartAll } from "../commands/upgrade.js";
import { preflightUpRoot } from "../commands/up/root-preflight.js";
import { applyReviewedGrant } from "../commands/credential-grants.js";
import { continuityFixture } from "./private-activation-fixtures.js";
import { PRIVATE_PREFIX as P } from "./private-activation.js";
import type { Execute } from "./deployment-target.js";

function setup(annotations: Record<string, string> = {}) {
  const namespace = { kind: "Namespace", metadata: { name: "kars-system", uid: "ns-uid", resourceVersion: "1", annotations } };
  const deployment = { kind: "Deployment", metadata: {
    name: "kars-controller", namespace: "kars-system", uid: "root-uid", resourceVersion: "1", annotations: {} as Record<string, string>,
  }, spec: { template: { metadata: { annotations: {} as Record<string, string> }, spec: {
    containers: [{ name: "controller", image: "mirror.azurecr.io/kars-controller:latest" }],
  } } } };
  const calls: { file: string; args: readonly string[] }[] = [];
  const execute: RootGuardExecute = async (file, args) => {
    calls.push({ file, args });
    if (args[0] === "get" && args[1] === "namespace") return { stdout: JSON.stringify(namespace) };
    if (args[0] === "get" && args[1] === "deployment") return { stdout: JSON.stringify(deployment) };
    if (args[0] === "get" && args[1] === "deployments") return { stdout: JSON.stringify({
      items: [{ metadata: { name: "agent", namespace: "kars-agent" } }],
    }) };
    return { stdout: "" };
  };
  return { namespace, deployment, calls, execute };
}

describe("private root refusal preflight", () => {
  it.each<Record<string, string>>([
    { [`${P}enabled`]: "true", [`${P}state`]: "Qualified" },
    { [`${P}state`]: "Pending" },
    { [`${P}root-retirement`]: '{"version":1,"phase":"restoring"}' },
    { [`${P}root-retirement`]: '{"version":2}' },
    { [`${P}root-retirement`]: "{malformed" },
    { [`${P}root-retirement`]: "" },
    { [`${P}root-retirement`]: '{"version":999}' },
    { [`${P}epoch`]: "a".repeat(64) },
    { [`${P}enabled`]: "false" },
  ])("refuses private evidence without granting meaning to malformed or incomplete receipts: %j", async annotations => {
    const f = setup(annotations);
    await expect(assertControllerMutationAllowed(f.execute)).rejects.toMatchObject({ reason: "protected" });
    expect(f.calls).toHaveLength(1);
    expect(f.calls.every(call => call.file === "kubectl" && call.args[0] === "get")).toBe(true);
  });

  it.each(["namespace label", "deployment annotation", "template epoch"])("does not lose %s evidence when namespace annotations are absent", async location => {
    const f = setup();
    if (location === "namespace label") Object.assign(f.namespace.metadata, { labels: { [`${P}enabled`]: "true" } });
    if (location === "deployment annotation") f.deployment.metadata.annotations[`${P}epoch`] = "old";
    if (location === "template epoch") f.deployment.spec.template.metadata.annotations[`${P}epoch`] = "old";
    await expect(assertControllerMutationAllowed(f.execute)).rejects.toBeInstanceOf(PrivateRootUpgradeBlocked);
    expect(f.calls.every(call => call.args[0] === "get")).toBe(true);
  });

  it.each(["null", "[]", "{}", "{broken", '{"metadata":{"name":"kars-system"}}',
    '{"metadata":{"name":"wrong","uid":"id","resourceVersion":"1"}}',
    '{"metadata":{"name":"kars-system","uid":"id","resourceVersion":"1","annotations":[]}}',
    '{"metadata":{"name":"kars-system","uid":"id","resourceVersion":"1","annotations":{"private":false}}}',
  ])("fails closed on malformed namespace evidence %s", async stdout => {
    await expect(assertControllerMutationAllowed(async () => ({ stdout }))).rejects.toMatchObject({ reason: "unavailable" });
  });

  it.each(["namespace", "deployment"])("redacts unavailable %s evidence and never retries", async kind => {
    const f = setup();
    const calls: string[] = [];
    const run: RootGuardExecute = async (file, args, options) => {
      calls.push(args[1]!);
      if (args[1] === kind) throw { stderr: "Error from server (Forbidden): PRIVATE_VALUE_SENTINEL", exitCode: 1,
        message: "PRIVATE_VALUE_SENTINEL" };
      return f.execute(file, args, options);
    };
    const error = await assertControllerMutationAllowed(run).catch(error => error);
    expect(error).toBeInstanceOf(PrivateRootUpgradeBlocked);
    expect(error.reason).toBe("unavailable");
    expect(String(error)).toContain("Forbidden");
    expect(JSON.stringify(error)).not.toContain("PRIVATE_VALUE_SENTINEL");
    expect(String(error)).not.toContain("PRIVATE_VALUE_SENTINEL");
    expect(calls.filter(item => item === kind)).toHaveLength(1);
  });

  it("accepts successful absence and a fully readable unqualified root", async () => {
    await expect(assertControllerMutationAllowed(async () => ({ stdout: "" }))).resolves.toBeUndefined();
    const f = setup();
    await expect(assertControllerMutationAllowed(f.execute)).resolves.toBeUndefined();
    expect(f.calls.map(call => call.args)).toEqual([
      ["get", "namespace", "kars-system", "--ignore-not-found", "-o", "json"],
      ["get", "deployment", "kars-controller", "-n", "kars-system", "--ignore-not-found", "-o", "json"],
    ]);
  });

  it("rejects publication to a mutable image actually used by the protected root, not by component-name guessing", async () => {
    const f = setup({ [`${P}state`]: "Qualified" });
    await expect(assertControllerMutationAllowed(f.execute, "kars-system", ["mirror.azurecr.io/kars-controller:latest"]))
      .rejects.toMatchObject({ reason: "protected" });
    f.deployment.spec.template.spec.containers[0]!.image = "mirror.azurecr.io/custom";
    await expect(assertControllerMutationAllowed(f.execute, "kars-system", ["mirror.azurecr.io/custom:latest"]))
      .rejects.toMatchObject({ reason: "protected" });
    f.deployment.spec.template.spec.containers[0]!.image = "MIRROR.azurecr.io:443/kars-controller:latest";
    await expect(assertControllerMutationAllowed(f.execute, "kars-system", ["mirror.azurecr.io/kars-controller:latest"]))
      .rejects.toMatchObject({ reason: "protected" });
  });

  it("allows unrelated or digest-pinned publication without authorizing a root restart", async () => {
    const f = setup({ [`${P}state`]: "Qualified" });
    await expect(assertControllerMutationAllowed(f.execute, "kars-system", ["mirror.azurecr.io/openclaw-sandbox:latest"]))
      .resolves.toBeUndefined();
    await expect(assertControllerMutationAllowed(f.execute, "kars-system", ["different.azurecr.io/kars-controller:latest"]))
      .resolves.toBeUndefined();
    f.deployment.spec.template.spec.containers[0]!.image += `@sha256:${"a".repeat(64)}`;
    await expect(assertControllerMutationAllowed(f.execute, "kars-system", ["mirror.azurecr.io/kars-controller:latest"]))
      .resolves.toBeUndefined();
    await expect(assertControllerMutationAllowed(f.execute)).rejects.toMatchObject({ reason: "protected" });
  });

  it("includes root init-container images in publication checks", async () => {
    const f = setup({ [`${P}state`]: "Qualified" });
    Object.assign(f.deployment.spec.template.spec, { initContainers: [
      { name: "bootstrap", image: "mirror.azurecr.io/openclaw-sandbox:latest" },
    ] });
    await expect(assertControllerMutationAllowed(f.execute, "kars-system", ["mirror.azurecr.io/openclaw-sandbox:latest"]))
      .rejects.toMatchObject({ reason: "protected" });
  });

  it("does not probe a root for bridge-only and sandbox-only rendered workloads", async () => {
    const run: RootGuardExecute = async () => { throw new Error("Root must not be consulted"); };
    await assertRenderedControllersMutable(run, [
      { kind: "Deployment", metadata: { name: "kars-bridge-bff", namespace: "bridge" } },
      { kind: "Deployment", metadata: { name: "agent", namespace: "kars-agent" } },
    ], "bridge");
  });

  it("uses the actual rendered controller namespace, not the Helm release namespace", async () => {
    const seen: string[] = [];
    await assertRenderedControllersMutable(async (_file, args) => { seen.push(args[2]!); return { stdout: "" }; }, [
      { kind: "Deployment", metadata: { name: "kars-controller", namespace: "actual-root" } },
    ], "release-namespace");
    expect(seen).toEqual(["actual-root"]);
  });

  it("refuses controller and combined rollouts before even restarting the mesh", async () => {
    for (const operation of [restartController, (run: Execute) => rolloutRestartAll(run, { kind: "absent", namespace: "agentmesh", deployments: [] })]) {
      const f = setup({ [`${P}state`]: "Qualified" });
      await expect(operation(f.execute as unknown as Execute)).rejects.toBeInstanceOf(PrivateRootUpgradeBlocked);
      expect(f.calls.every(call => call.args[0] === "get")).toBe(true);
    }
  });

  it("preserves standalone sandbox rollout arguments without changing the protected root", async () => {
    const f = setup({ [`${P}state`]: "Qualified" });
    await restartSandboxes(f.execute as unknown as Execute);
    expect(f.calls.map(call => call.args)).toEqual([
      ["get", "deployments", "-A", "-l", "kars.azure.com/component=sandbox", "-o", "json"],
      ["rollout", "restart", "deployment/agent", "-n", "kars-agent"],
      ["rollout", "status", "deployment/agent", "-n", "kars-agent", "--timeout=300s"],
    ]);
  });

  it("keeps original restoring/no-grant credential recovery available without changing its binding", async () => {
    const f = continuityFixture();
    let failSeal = true;
    const execute = async (args: string[], input?: string) => {
      if (args[0] === "patch" && args[1] === "namespace" && args[2] === "core") {
        const patch = JSON.parse(args[args.indexOf("-p") + 1]!);
        const receipt = patch.metadata?.annotations?.[`${P}root-retirement`];
        if (failSeal && receipt && JSON.parse(receipt).version === 2) throw new Error("interrupt seal");
      }
      return f.execute(args, input);
    };
    await expect(applyReviewedGrant(execute, await f.document("work", [], execute))).rejects.toThrow("interrupt seal");
    const before = f.namespace("core").metadata.annotations[`${P}root-retirement`];
    expect(JSON.parse(before).phase).toBe("restoring");
    const run: RootGuardExecute = async (_file, args) => ({ stdout: await execute([...args]) });
    await expect(assertControllerMutationAllowed(run, "core")).rejects.toMatchObject({ reason: "protected" });
    failSeal = false;
    await applyReviewedGrant(execute, await f.document("work", [], execute));
    expect(JSON.parse(f.namespace("core").metadata.annotations[`${P}root-retirement`]).retirement).toBe(before);
    expect(f.grant().spec.privateActivation.phase).toBe("qualified");
  });
});

describe("explicit fenced Helm controller no-op", () => {
  function fixture() {
    const base = setup({ [`${P}root-retirement`]: '{"version":2}' });
    const live = {
      ...base.deployment, apiVersion: "apps/v1",
      metadata: { ...base.deployment.metadata, labels: { "app.kubernetes.io/managed-by": "Helm" },
        annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "kars-system" } },
    };
    live.spec.template.spec.containers[0]!.image += `@sha256:${"a".repeat(64)}`;
    const desired = structuredClone(live);
    const calls: { args: readonly string[]; input?: string }[] = [];
    let afterPreview = () => {};
    const execute: RootGuardExecute = async (_file, args, options) => {
      calls.push({ args, input: options.input });
      if (args[0] === "get" && args[1] === "namespace") return { stdout: JSON.stringify(base.namespace) };
      if (args[0] === "get" && args[1] === "deployment") return { stdout: JSON.stringify(live) };
      if (args[0] === "apply" && args.includes("--dry-run=server")) {
        const request = JSON.parse(options.input!);
        const stdout = JSON.stringify({ ...structuredClone(live), ...request });
        afterPreview();
        return { stdout };
      }
      throw new Error("Unreviewed operation");
    };
    const run = (previous: unknown[] = []) =>
      assertRenderedControllersMutable(execute, [desired], "kars-system", previous, true);
    return { live, desired, namespace: base.namespace, execute, calls, run,
      afterPreview(callback: () => void) { afterPreview = callback; } };
  }

  it("requires explicit Helm SSA opt-in and still refuses restart operations", async () => {
    const f = fixture();
    await expect(assertRenderedControllersMutable(f.execute, [f.desired], "kars-system"))
      .rejects.toMatchObject({ reason: "protected" });
    await expect(assertControllerMutationAllowed(f.execute)).rejects.toMatchObject({ reason: "protected" });
    expect(f.calls.every(call => call.args[0] === "get")).toBe(true);
  });

  it("admits only a real no-op preview with a UID/RV fence and no real writes", async () => {
    const f = fixture();
    const historical = structuredClone(f.desired);
    historical.spec.template.spec.containers[0]!.image = "old:latest";
    await expect(f.run([historical])).resolves.toBeUndefined();
    const preview = f.calls.find(call => call.args[0] === "apply")!;
    expect(preview.args).toEqual(["apply", "--server-side", "--field-manager=helm", "--dry-run=server", "-f", "-", "-o", "json"]);
    expect(JSON.parse(preview.input!).metadata).toMatchObject({ uid: "root-uid", resourceVersion: "1" });
    expect(f.calls.filter(call => call.args[0] !== "get")).toEqual([preview]);
  });

  it.each(["uid", "resourceVersion"] as const)("refuses missing/stale %s before admission", async key => {
    for (const value of ["", "changed"]) {
      const f = fixture();
      f.desired.metadata[key] = value;
      await expect(f.run()).rejects.toMatchObject({ reason: "protected" });
      expect(f.calls.every(call => call.args[0] === "get")).toBe(true);
    }
  });

  it.each(["spec", "labels", "annotations", "owner", "mutable-image", "mutable-init"] as const)(
    "refuses %s changes", async change => {
      const f = fixture();
      if (change === "spec") f.desired.spec.template.spec.containers[0]!.image = `new:latest@sha256:${"b".repeat(64)}`;
      if (change === "labels") Object.assign(f.desired.metadata.labels, { injected: "value" });
      if (change === "annotations") Object.assign(f.desired.metadata.annotations, { [`${P}epoch`]: "replacement" });
      if (change === "owner") f.live.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
      if (change === "mutable-image") f.live.spec.template.spec.containers[0]!.image = "controller:latest";
      if (change === "mutable-init") Object.assign(f.live.spec.template.spec, { initContainers: [{ name: "init", image: "init:latest" }] });
      await expect(f.run()).rejects.toMatchObject({ reason: "protected" });
      expect(f.calls.every(call => call.args[0] === "get" || call.args.includes("--dry-run=server"))).toBe(true);
    });

  it.each(["controller-version", "controller-uid", "namespace-evidence"] as const)(
    "rejects concurrent %s changes after a successful preview", async change => {
      const f = fixture();
      f.afterPreview(() => {
        if (change === "controller-version") f.live.metadata.resourceVersion = "2";
        if (change === "controller-uid") f.live.metadata.uid = "replacement";
        if (change === "namespace-evidence") f.namespace.metadata.annotations[`${P}epoch`] = "replacement";
      });
      await expect(f.run()).rejects.toMatchObject({ reason: "protected" });
    });

  it("refuses deletion and ambiguous duplicate targets", async () => {
    const f = fixture();
    await expect(assertRenderedControllersMutable(f.execute, [], "kars-system", [f.desired], true))
      .rejects.toMatchObject({ reason: "protected" });
    await expect(assertRenderedControllersMutable(f.execute, [f.desired, f.desired], "kars-system", [], true))
      .rejects.toMatchObject({ reason: "protected" });
    expect(f.calls.every(call => call.args[0] === "get")).toBe(true);
  });

  it.each(["desired", "historical"])("refuses a separately revision-bound TLS Secret in the %s release", async location => {
    const f = fixture();
    Object.assign(f.namespace.metadata.annotations, {
      [`${P}budget-namespace`]: "kars-system", [`${P}budget-tls-name`]: "budget-tls", [`${P}budget-tls-version`]: "7",
    });
    const secret = { apiVersion: "v1", kind: "Secret", metadata: { name: "budget-tls", namespace: "kars-system" } };
    await expect(assertRenderedControllersMutable(f.execute,
      [f.desired, ...(location === "desired" ? [secret] : [])], "kars-system",
      location === "historical" ? [secret] : [], true)).rejects.toMatchObject({ reason: "protected" });
    expect(f.calls.every(call => call.args[0] === "get")).toBe(true);
  });

  it.each(["admission", "invalid-response"])("fails closed and redacts %s failures", async failure => {
    const f = fixture();
    const execute: RootGuardExecute = (file, args, options) => {
      if (args[0] === "apply") {
        if (failure === "admission") throw new Error("PRIVATE_VALUE_SENTINEL");
        return Promise.resolve({ stdout: "PRIVATE_VALUE_SENTINEL" });
      }
      return f.execute(file, args, options);
    };
    const error = await assertRenderedControllersMutable(execute, [f.desired], "kars-system", [], true).catch(error => error);
    expect(error).toMatchObject({ reason: "unavailable" });
    expect(String(error)).not.toContain("PRIVATE_VALUE_SENTINEL");
  });
});

describe("full up pre-effect root check", () => {
  it("moves existing-cluster checks before provisioning and uses the old connection arguments", async () => {
    const f = setup({ [`${P}state`]: "Qualified" });
    const calls: { file: string; args: readonly string[] }[] = [];
    const run: RootGuardExecute = async (file, args, options) => {
      calls.push({ file, args });
      if (file === "az") return { stdout: args[1] === "show" ? "Succeeded" : "" };
      return f.execute(file, args, options);
    };
    await expect(preflightUpRoot(run, "rg", "target-aks", false)).rejects.toMatchObject({ reason: "protected" });
    expect(calls.map(call => call.args)).toEqual([
      ["aks", "show", "-g", "rg", "-n", "target-aks", "--query", "provisioningState", "-o", "tsv"],
      ["aks", "get-credentials", "--name", "target-aks", "--resource-group", "rg", "--overwrite-existing", "--output", "none"],
      ["get", "namespace", "kars-system", "--ignore-not-found", "-o", "json"],
    ]);
  });

  it("allows a proven absent AKS without consulting an unrelated ambient Kubernetes context", async () => {
    const calls: string[] = [];
    const run: RootGuardExecute = async (file) => {
      calls.push(file);
      throw { stderr: "(ResourceNotFound) The AKS is absent" };
    };
    expect(await preflightUpRoot(run, "rg", "fresh-aks", false)).toBe(false);
    expect(calls).toEqual(["az"]);
  });

  it.each(["Forbidden", "timeout", "malformed", "credentials"])("does not treat %s as a fresh cluster", async reason => {
    const run: RootGuardExecute = async (_file, args) => {
      if (reason === "malformed") return { stdout: "unrecognized" };
      if (reason === "credentials" && args[1] === "show") return { stdout: "Succeeded" };
      throw new Error(reason);
    };
    await expect(preflightUpRoot(run, "rg", "target-aks", false)).rejects.toMatchObject({ reason: "unavailable" });
  });

  it("does not confuse an error's resource name or conflicting reasons with authoritative absence", async () => {
    for (const error of [
      { stderr: "ERROR: (Forbidden) Cannot read resource group '(ResourceNotFound)'" },
      { stderr: "ERROR: (ResourceNotFound) absent\nERROR: (Forbidden) denied" },
      new Error("Command failed for '(ResourceNotFound)'"),
    ]) {
      await expect(preflightUpRoot(async () => { throw error; }, "rg", "target-aks", false))
        .rejects.toMatchObject({ reason: "unavailable" });
    }
  });
});
