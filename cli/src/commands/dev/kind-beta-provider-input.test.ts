// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import type { Execute } from "../../lib/private-activation.js";
import { publishKindBetaProviderInput } from "./kind-beta-provider-input.js";

function fixture() {
  const grant: any = { metadata: { name: "workspace", uid: "grant", generation: 2, resourceVersion: "7" },
    spec: { enabled: true, workspaceUid: "namespace", integrationStores: [
      { purpose: "providers", secret: { name: "kars-inference-providers", uid: "store" } },
    ] }, status: { phase: "Ready", observedGeneration: 2, conditions: ["Ready", "WriterReady", "IntegrationReady", "PrivateConsumptionReady"]
      .map(type => ({ type, status: "True" })) } };
  const store: any = { type: "Opaque", metadata: { name: "kars-inference-providers", namespace: "kars-system", uid: "store", resourceVersion: "8",
    annotations: { "kars.azure.com/credential-store-grant-uid": "grant" } } };
  const namespace: any = { metadata: { name: "kars-system", uid: "namespace" } };
  const input: any = { apiVersion: "v1", kind: "Secret", type: "Opaque", metadata: { name: store.metadata.name, namespace: "kars-system" },
    stringData: { KARS_PROVIDER_OPENAI_ENDPOINT: "https://api.openai.com/v1", KARS_PROVIDER_OPENAI_API_KEY: "private-fixture-only" } };
  const execute = vi.fn<Execute>(async (args, payload) => {
    if (args[0] === "wait") return "";
    if (args[0] === "get") return JSON.stringify(args[1] === "secret" ? store : args[1] === "namespace" ? namespace : grant);
    if (args[0] === "patch") {
      const patch = JSON.parse(payload!);
      expect(patch.slice(0, 2)).toEqual([
        { op: "test", path: "/metadata/uid", value: "store" },
        { op: "test", path: "/metadata/resourceVersion", value: "8" },
      ]);
      store.data = patch[2].value;
      return "";
    }
    throw new Error("Unexpected command");
  });
  return { grant, store, namespace, input, execute, run: () => publishKindBetaProviderInput(input, execute) };
}

describe("Kind named-provider enrollment handoff", () => {
  it("fills only the live enrolled empty store, using stdin and an exact identity/version compare", async () => {
    const f = fixture();
    const metadata = structuredClone(f.store.metadata);
    await f.run();
    expect(f.store.metadata).toEqual(metadata);
    expect(f.store.data).toEqual(Object.fromEntries(Object.entries(f.input.stringData).map(([key, value]) => [key, Buffer.from(value as string).toString("base64")])));
    const writes = f.execute.mock.calls.filter(([args]) => args[0] === "patch");
    expect(writes).toHaveLength(1);
    expect(writes[0][0]).toContain("--patch-file=/dev/stdin");
    expect(JSON.stringify(f.execute.mock.calls.map(([args]) => args))).not.toContain("private-fixture-only");
    expect(f.execute.mock.calls.findIndex(([args]) => args.includes("--for=condition=PrivateConsumptionReady"))).toBeLessThan(f.execute.mock.calls.findIndex(([args]) => args[0] === "patch"));
    expect(f.execute.mock.calls.at(-1)?.[0][0]).toBe("get");
  });
  it("uses only the authenticated writer for publication and keeps readiness reads on the operator", async () => {
    const f = fixture(); const normal = f.execute.getMockImplementation()!;
    const write = vi.fn<Execute>(async (args, input) => {
      expect(args[0]).toBe("patch"); return normal(args, input);
    });
    await publishKindBetaProviderInput(f.input, f.execute, write);
    expect(write).toHaveBeenCalledOnce();
    expect(f.execute.mock.calls.every(([args]) => args[0] !== "patch")).toBe(true);
    expect(f.store.data.KARS_PROVIDER_OPENAI_API_KEY).toBe(Buffer.from("private-fixture-only").toString("base64"));
    expect(f.execute.mock.calls.at(-1)?.[0][0]).toBe("get");
  });
  it.each([
    ["replaced store", (f: ReturnType<typeof fixture>) => { f.store.metadata.uid = "replacement"; }],
    ["disabled grant", (f: ReturnType<typeof fixture>) => { f.grant.spec.enabled = false; }],
    ["stale reconciliation", (f: ReturnType<typeof fixture>) => { f.grant.status.observedGeneration = 1; }],
    ["missing private readiness", (f: ReturnType<typeof fixture>) => { f.grant.status.conditions.pop(); }],
    ["replaced namespace", (f: ReturnType<typeof fixture>) => { f.namespace.metadata.uid = "replacement"; }],
    ["terminating namespace", (f: ReturnType<typeof fixture>) => { f.namespace.metadata.deletionTimestamp = "now"; }],
    ["wrong purpose", (f: ReturnType<typeof fixture>) => { f.grant.spec.integrationStores[0].purpose = "github-app"; }],
    ["duplicate enrollment", (f: ReturnType<typeof fixture>) => { f.grant.spec.integrationStores.push(f.grant.spec.integrationStores[0]); }],
    ["wrong grant annotation", (f: ReturnType<typeof fixture>) => { f.store.metadata.annotations["kars.azure.com/credential-store-grant-uid"] = "other"; }],
    ["concurrent credentials", (f: ReturnType<typeof fixture>) => { f.store.data = { retained: "secret" }; }],
    ["injected router settings", (f: ReturnType<typeof fixture>) => { f.input.stringData.HTTPS_PROXY = "https://unreviewed.example"; }],
  ] as const)("preserves state on %s", async (_name, mutate) => {
    const f = fixture(); mutate(f);
    const before = structuredClone(f.store);
    await expect(f.run()).rejects.toThrow();
    expect(f.store).toEqual(before);
    expect(f.execute.mock.calls.some(([args]) => args[0] === "patch")).toBe(false);
  });
  it("does not retry a concurrent write rejected by Kubernetes", async () => {
    const f = fixture(), normal = f.execute.getMockImplementation()!;
    f.execute.mockImplementation(async (args, payload) => {
      if (args[0] === "patch") throw new Error("resourceVersion conflict");
      return normal(args, payload);
    });
    await expect(f.run()).rejects.toThrow("resourceVersion conflict");
    expect(f.execute.mock.calls.filter(([args]) => args[0] === "patch")).toHaveLength(1);
    expect(f.store.data).toBeUndefined();
  });
  it("fails if the persisted data differs instead of reporting success", async () => {
    const f = fixture(), normal = f.execute.getMockImplementation()!;
    f.execute.mockImplementation(async (args, payload) => {
      const result = await normal(args, payload);
      if (args[0] === "patch") f.store.data = {};
      return result;
    });
    await expect(f.run()).rejects.toThrow("Installed provider inputs changed");
  });
});
