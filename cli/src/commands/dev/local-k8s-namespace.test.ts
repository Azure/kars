// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { parseAllDocuments } from "yaml";
import { autoCreateSandbox, type LocalK8sOptions } from "./local-k8s.js";
import { CLAIM, namespacePrestaged, type OwnershipObject } from "../../lib/namespace-ownership.js";

const { execute } = vi.hoisted(() => ({
  execute: vi.fn<(file: string, args: readonly string[], options?: { input?: string }) => Promise<{ stdout: string }>>(),
}));
vi.mock("execa", () => ({ execa: execute }));
vi.mock("../../config.js", () => ({
  loadConfig: vi.fn(),
  getSecret: (name: string) => name === "telegram-token" ? "test-channel-token" : undefined,
}));
vi.mock("../../refs.js", () => ({ loadAgtProfile: () => "version: 1\n" }));

const tools = {
  kind: "kind", kubectl: "/test/bin/kubectl", helm: "helm",
  runtime: "docker", runtimeName: "docker" as const, env: {},
};
const options: LocalK8sOptions = {
  name: "demo", clusterName: "isolated-test", image: "example.test/sandbox:latest",
  ephemeral: false, noBuild: true, channels: "telegram",
};
const credentials = { endpoint: "https://example.test", model: "test-model", apiKey: "" };

function cluster() {
  const state = {
    sandboxes: [] as OwnershipObject[],
    namespace: undefined as OwnershipObject | undefined,
    applied: [] as OwnershipObject[],
    error: "",
  };
  execute.mockImplementation(async (file, args, commandOptions) => {
    expect(file).toBe(tools.kubectl);
    expect(args.slice(0, 2)).toEqual(["--context", "kind-isolated-test"]);
    const operation = args[2];
    if ((operation === "get" && state.error === "Forbidden (403)")
      || (operation === "create" && state.error === "AlreadyExists (409)")) {
      throw new Error(state.error);
    }
    if (operation === "get") {
      const result = args[3] === "karssandboxes" ? { items: state.sandboxes } : state.namespace;
      return { stdout: result ? JSON.stringify(result) : "" };
    }
    if (operation === "create") {
      expect(state.namespace).toBeUndefined();
      state.namespace = JSON.parse(commandOptions!.input!) as OwnershipObject;
      state.namespace.metadata.uid = "reserved-uid";
      state.namespace.metadata.resourceVersion = "1";
      state.namespace.metadata.creationTimestamp = "2026-09-07T10:00:00Z";
      return { stdout: JSON.stringify(state.namespace) };
    }
    if (operation === "apply") {
      state.applied = parseAllDocuments(commandOptions!.input!).map(document => {
        if (document.errors.length) throw document.errors[0];
        return document.toJSON();
      }).filter(Boolean);
      return { stdout: "" };
    }
    throw new Error(`Unexpected operation ${operation}`);
  });
  return state;
}

beforeEach(() => { execute.mockReset(); });

describe("local-k8s first-party namespace producer", () => {
  it("reserves atomically before credentials and puts the exact UID on the Sandbox", async () => {
    const state = cluster();
    await autoCreateSandbox(tools, options, credentials);
    expect(execute.mock.calls.map(([, args]) => args[2])).toEqual(["get", "get", "create", "apply"]);
    expect(state.applied.some(resource => resource.kind === "Namespace")).toBe(false);
    const sandbox = state.applied.find(resource => resource.kind === "KarsSandbox")!;
    sandbox.metadata.uid = "sandbox-uid";
    sandbox.metadata.resourceVersion = "2";
    sandbox.metadata.creationTimestamp = "2026-09-07T10:00:00Z";
    expect(sandbox.metadata.annotations?.[CLAIM.namespaceUid]).toBe("reserved-uid");
    expect(namespacePrestaged(state.namespace!, sandbox)).toBe(true);
    const secret = state.applied.find(resource => resource.kind === "Secret")!;
    expect(secret.metadata).toMatchObject({ name: "demo-credentials", namespace: "kars-demo" });
    expect(state.applied.indexOf(secret)).toBeLessThan(state.applied.indexOf(sandbox));

    state.sandboxes = [sandbox];
    state.namespace!.metadata.annotations![CLAIM.uid] = "sandbox-uid";
    delete state.namespace!.metadata.annotations![CLAIM.prestage];
    await autoCreateSandbox(tools, options, credentials);
    expect(execute.mock.calls.filter(([, args]) => args[2] === "create")).toHaveLength(1);
    expect(state.namespace!.metadata.uid).toBe("reserved-uid");
  });

  it("resumes only the explicit reservation after an interrupted creation", async () => {
    const state = cluster();
    await autoCreateSandbox(tools, options, credentials);
    await autoCreateSandbox(tools, options, credentials);
    expect(execute.mock.calls.filter(([, args]) => args[2] === "create")).toHaveLength(1);
    expect(state.applied.find(resource => resource.kind === "KarsSandbox")?.metadata.annotations)
      .toEqual({ [CLAIM.namespaceUid]: "reserved-uid" });
  });

  it("never stages credentials into an arbitrary pre-existing namespace", async () => {
    const state = cluster();
    state.namespace = { metadata: { name: "kars-demo", uid: "customer-uid", resourceVersion: "1" } };
    await expect(autoCreateSandbox(tools, options, credentials)).rejects.toThrow("explicit adoption");
    expect(execute.mock.calls.every(([, args]) => args[2] === "get")).toBe(true);
  });

  it("rejects a same-name Sandbox in another workspace before any writes", async () => {
    const state = cluster();
    state.sandboxes = [{
      metadata: { name: "demo", namespace: "other", uid: "other-uid", resourceVersion: "1" },
    }];
    await expect(autoCreateSandbox(tools, options, credentials)).rejects.toThrow("another workspace");
    expect(execute.mock.calls).toHaveLength(1);
  });

  it.each(["Forbidden (403)", "AlreadyExists (409)"])("surfaces %s without applying credentials", async error => {
    const state = cluster();
    state.error = error;
    await expect(autoCreateSandbox(tools, options, credentials)).rejects.toThrow(error);
    expect(execute.mock.calls.some(([, args]) => args[2] === "apply")).toBe(false);
  });
});
