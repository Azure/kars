// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import type { Execute } from "./deployment-target.js";
import {
  applySourceSandbox, prepareCredentialSource, updateCredentialSource,
  updateDirectCredentials, verifyCredentialReference, waitForCredentialSource,
} from "./credential-source.js";
import {
  SOURCE, SOURCE_KEYS, decode, removedKeys, targetName, validateValues,
  type Sandbox, type Secret,
} from "./credential-source-io.js";
import { CLAIM } from "./namespace-ownership.js";

const encode = (values: Record<string, string>) => Object.fromEntries(
  Object.entries(values).map(([key, value]) => [key, Buffer.from(value).toString("base64")]),
);
const reference = { name: "kars-credential-source-demo", uid: "source-uid" };
const opts = (extra = {}) => ({ updates: {}, remove: [], ...extra });

function sandbox(): Sandbox {
  return {
    metadata: { name: "demo", namespace: "kars-system", uid: "sandbox-uid", resourceVersion: "10",
      annotations: { [CLAIM.namespaceUid]: "namespace-uid" } },
    spec: { runtime: { kind: "OpenClaw", openclaw: {} } },
  };
}

function source(): Secret {
  return {
    apiVersion: "v1", kind: "Secret", type: "Opaque",
    metadata: {
      name: reference.name, namespace: "kars-system", uid: reference.uid, resourceVersion: "20",
      annotations: {
        [SOURCE.purpose]: "agent-source-v1", [SOURCE.target]: "demo",
        [SOURCE.workspace]: "kars-system", [SOURCE.intent]: "explicit-reference-v1",
        "customer.example/note": "preserve",
      },
    },
    data: encode({ TELEGRAM_BOT_TOKEN: "initial-source" }),
  };
}

function cluster() {
  const state = {
    sandbox: sandbox() as Sandbox | undefined,
    source: undefined as Secret | undefined,
    namespace: {
      metadata: {
        name: "kars-demo", uid: "namespace-uid", resourceVersion: "30",
        annotations: {
          [CLAIM.version]: "v1", [CLAIM.name]: "demo", [CLAIM.namespace]: "kars-system",
          [CLAIM.uid]: "sandbox-uid",
        },
      },
    } as { metadata: { name: string; uid: string; resourceVersion: string; annotations: Record<string, string> } } | undefined,
    legacy: {
      apiVersion: "v1", kind: "Secret", type: "Opaque",
      metadata: { name: "demo-credentials", namespace: "kars-demo", uid: "legacy-uid", resourceVersion: "40",
        annotations: { "customer.example/keep": "yes" } },
      data: encode({ TELEGRAM_BOT_TOKEN: "legacy-token", SLACK_BOT_TOKEN: "keep-slack" }),
    } as Secret | undefined,
    failRead: false, failWrite: false, pruneRef: false, acknowledge: true, revision: 100,
    recreateSourceOnPatch: false, conflictCreate: false,
  };
  const run = vi.fn(async (command: string, args: readonly string[], options?: { input?: string }) => {
    expect(command).toBe("kubectl");
    expect(args.some(arg => arg.startsWith("--from-literal"))).toBe(false);
    if (args[0] === "get") {
      if (state.failRead) throw Object.assign(new Error("forbidden: private-token-value"), { exitCode: 1 });
      let object: unknown;
      if (args[1] === "karssandbox") {
        if (state.acknowledge && state.sandbox?.spec.credentialsRef && state.source) {
          state.sandbox.status = { conditions: [{
            type: "CredentialsReady", status: "True", reason: "Projected",
            message: JSON.stringify({ sourceUid: state.source.metadata.uid, sourceVersion: state.source.metadata.resourceVersion }),
          }] };
        }
        object = state.sandbox;
      } else if (args[1] === "namespace") object = state.namespace;
      else object = args[2] === reference.name ? state.source : state.legacy;
      if (!object) return { stdout: "" };
      if (args.includes("jsonpath={.metadata}")) object = (object as Secret).metadata;
      return { stdout: JSON.stringify(object) };
    }
    if (state.failWrite) throw Object.assign(new Error(`admission echoed ${options?.input}`), { exitCode: 1 });
    const body = JSON.parse(options!.input!);
    if (args[0] === "create") {
      if (state.conflictCreate) throw Object.assign(new Error("AlreadyExists"), { exitCode: 1 });
      if (body.kind === "Secret") {
        expect(state.source).toBeUndefined();
        body.metadata.uid = reference.uid;
        body.metadata.resourceVersion = String(++state.revision);
        state.source = body;
      } else {
        expect(body.kind).toBe("KarsSandbox");
        expect(state.sandbox).toBeUndefined();
        body.metadata.uid = "sandbox-created";
        body.metadata.resourceVersion = String(++state.revision);
        if (state.pruneRef) delete body.spec.credentialsRef;
        state.sandbox = body;
      }
      return { stdout: JSON.stringify(body) };
    }
    expect(args[0]).toBe("patch");
    expect(args).toContain("--patch-file=/dev/stdin");
    const object = args[1] === "karssandbox" ? state.sandbox
      : args[2] === reference.name ? state.source : state.legacy;
    expect(object).toBeDefined();
    if (state.recreateSourceOnPatch && object === state.source) {
      object!.metadata.uid = "replacement";
      object!.metadata.resourceVersion = "replacement-version";
    }
    if (object!.metadata.uid !== body.metadata.uid || object!.metadata.resourceVersion !== body.metadata.resourceVersion) {
      throw Object.assign(new Error("Conflict"), { exitCode: 1 });
    }
    if (args[1] === "karssandbox") {
      if (state.pruneRef) delete body.spec.credentialsRef;
      if (body.spec.credentialsRef === null) delete state.sandbox!.spec.credentialsRef;
      else state.sandbox!.spec = { ...state.sandbox!.spec, ...body.spec };
    } else {
      const secret = object as Secret;
      secret.data ??= {};
      for (const [key, value] of Object.entries(body.data)) {
        if (value === null) delete secret.data[key];
        else secret.data[key] = value as string;
      }
    }
    object!.metadata.resourceVersion = String(++state.revision);
    return { stdout: JSON.stringify(object) };
  });
  return { state, run, execute: run as unknown as Execute };
}

describe("credential source contract", () => {
  it("matches the core and existing handoff agent-key allowlist", () => {
    const rust = readFileSync(new URL("../../../controller/src/credential_source.rs", import.meta.url), "utf8");
    const block = rust.slice(rust.indexOf("pub const AGENT_KEYS"), rust.indexOf("pub fn source_name"));
    expect([...block.matchAll(/"([A-Z][A-Z0-9_]+)"/g)].map(match => match[1])).toEqual([...SOURCE_KEYS]);
    expect(SOURCE_KEYS).not.toContain("OPENAI_API_KEY");
  });

  it.each(["OPENAI_API_KEY", "AZURE_CLIENT_SECRET", "AGT_KEY", "NODE_OPTIONS", "PATH"])(
    "rejects reserved/provider key %s without echoing values", key => {
      expect(() => validateValues({ [key]: "private-value" })).toThrow("supported agent");
      try { validateValues({ [key]: "private-value" }); }
      catch (error) { expect(String(error)).not.toContain("private-value"); }
    },
  );

  it("normalizes removal keys and rejects path-shaped targets", () => {
    expect(removedKeys("telegram-token,SLACK_BOT_TOKEN")).toEqual(["TELEGRAM_BOT_TOKEN", "SLACK_BOT_TOKEN"]);
    expect(() => targetName("../other", "kars-system")).toThrow("DNS");
    expect(() => validateValues({ TELEGRAM_BOT_TOKEN: "a\0b" })).toThrow("environment");
  });
});

describe("real source creation and migration", () => {
  it("creates the source before the CR, pins its returned UID, and never writes a runtime namespace", async () => {
    const { state, run, execute } = cluster();
    state.sandbox = undefined; state.namespace = undefined; state.legacy = undefined;
    const prepared = await prepareCredentialSource(execute, "demo", "kars-system", { TELEGRAM_BOT_TOKEN: "sensitive-token-123" });
    expect(prepared.reference).toEqual(reference);
    expect(state.sandbox).toBeUndefined();
    await applySourceSandbox(execute, {
      apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox",
      metadata: { name: "demo", namespace: "kars-system" }, spec: { runtime: { kind: "OpenClaw", openclaw: {} } },
    }, prepared);
    expect(state.sandbox).toMatchObject({ spec: { credentialsRef: reference } });
    const creates = run.mock.calls.filter(([, args]) => args[0] === "create");
    expect(creates).toHaveLength(2);
    expect(JSON.parse(creates[0][2]!.input!).kind).toBe("Secret");
    expect(JSON.parse(creates[1][2]!.input!).kind).toBe("KarsSandbox");
    expect(run.mock.calls.some(([, args]) => args.includes("kars-demo"))).toBe(false);
    expect(run.mock.calls.some(([, args]) => args.join(" ").includes("sensitive-token-123"))).toBe(false);
  });

  it("migrates the complete legacy collection once without changing its object or UID", async () => {
    const { state, execute, run } = cluster();
    const legacy = structuredClone(state.legacy);
    const result = await updateCredentialSource(execute, "demo", "kars-system", opts({
      useSource: true, updates: { TELEGRAM_BOT_TOKEN: "rotated" },
    }));
    expect(result?.reference).toEqual(reference);
    expect(decode(state.source)).toEqual({ TELEGRAM_BOT_TOKEN: "rotated", SLACK_BOT_TOKEN: "keep-slack" });
    expect(state.legacy).toEqual(legacy);
    expect(state.sandbox?.spec.credentialsRef).toEqual(reference);
    expect(run.mock.calls.filter(([, args]) => args.includes("kars-demo")).every(([, args]) => args[0] === "get")).toBe(true);
  });

  it("refuses unsafe/incomplete legacy migration before any source write", async () => {
    const { state, execute, run } = cluster();
    state.legacy!.data!.OPENAI_API_KEY = Buffer.from("provider-value").toString("base64");
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts({ useSource: true }))).rejects.toThrow("supported agent");
    expect(run.mock.calls.every(([, args]) => args[0] === "get")).toBe(true);
    expect(state.source).toBeUndefined();
  });

  it("does not silently attach a source after a racing CR create409 or schema pruning", async () => {
    for (const prune of [false, true]) {
      const { state, execute } = cluster();
      state.sandbox = undefined; state.namespace = undefined;
      const prepared = await prepareCredentialSource(execute, "demo", "kars-system", {});
      state.pruneRef = prune; state.conflictCreate = !prune;
      await expect(applySourceSandbox(execute, {
        apiVersion: "kars.azure.com/v1alpha1", kind: "KarsSandbox",
        metadata: { name: "demo", namespace: "kars-system" }, spec: {},
      }, prepared)).rejects.toThrow();
    }
  });
});

describe("update, rotation, removal, and path selection", () => {
  it("retains the direct path when no reference exists", async () => {
    const { state, execute } = cluster();
    expect(await updateCredentialSource(execute, "demo", "kars-system", opts())).toBeUndefined();
    const uid = state.legacy!.metadata.uid;
    await updateDirectCredentials(execute, "demo", { OPENAI_API_KEY: "legacy-supported" }, ["TELEGRAM_BOT_TOKEN"]);
    expect(state.legacy!.metadata.uid).toBe(uid);
    expect(decode(state.legacy)).toEqual({ SLACK_BOT_TOKEN: "keep-slack", OPENAI_API_KEY: "legacy-supported" });
  });

  it("does not treat a wrong workspace or same-name conflict as the legacy direct path", async () => {
    const { state, execute, run } = cluster();
    state.namespace!.metadata.annotations[CLAIM.namespace] = "other-workspace";
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts())).rejects.toThrow();
    state.sandbox = undefined;
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts())).rejects.toThrow("--namespace");
    expect(run.mock.calls.every(([, args]) => args[0] === "get")).toBe(true);
  });

  it("updates/removes owned source keys without delete/recreate or legacy fallback", async () => {
    const { state, execute, run } = cluster();
    state.source = source(); state.sandbox!.spec.credentialsRef = reference;
    const legacy = structuredClone(state.legacy);
    await updateCredentialSource(execute, "demo", "kars-system", opts({
      updates: { SLACK_BOT_TOKEN: "new-slack" }, remove: ["TELEGRAM_BOT_TOKEN"],
    }));
    expect(decode(state.source)).toEqual({ SLACK_BOT_TOKEN: "new-slack" });
    expect(state.source!.metadata.uid).toBe(reference.uid);
    expect(state.source!.metadata.annotations!["customer.example/note"]).toBe("preserve");
    expect(state.legacy).toEqual(legacy);
    expect(run.mock.calls.some(([, args]) => args.includes("kars-demo") || args[0] === "delete")).toBe(false);
  });

  it("requires explicit --use-source when the pinned source incarnation was replaced", async () => {
    const { state, execute } = cluster();
    state.source = source(); state.sandbox!.spec.credentialsRef = reference;
    state.source.metadata.uid = "new-source";
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts())).rejects.toThrow("missing/replaced");
    await updateCredentialSource(execute, "demo", "kars-system", opts({ useSource: true }));
    expect(state.sandbox!.spec.credentialsRef?.uid).toBe("new-source");
  });

  it("disables the reference without deleting source/customer data and preserves no-restart on direct mode", async () => {
    const { state, execute } = cluster();
    state.source = source(); state.sandbox!.spec.credentialsRef = reference;
    const original = structuredClone(state.source);
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts({ restart: false }))).rejects.toThrow("--no-restart");
    await updateCredentialSource(execute, "demo", "kars-system", opts({ disableSource: true }));
    expect(state.sandbox!.spec.credentialsRef).toBeUndefined();
    expect(state.source).toEqual(original);
    expect(await updateCredentialSource(execute, "demo", "kars-system", opts({ restart: false }))).toBeUndefined();
  });

  it("supports explicit pre-CR staging without automatic delivery", async () => {
    const { state, execute } = cluster();
    state.sandbox = undefined; state.namespace = undefined;
    const result = await updateCredentialSource(execute, "demo", "kars-system", opts({
      useSource: true, updates: { TELEGRAM_BOT_TOKEN: "staged" },
    }));
    expect(result?.staged).toBe(true);
    expect(state.sandbox).toBeUndefined();
    expect(state.namespace).toBeUndefined();
  });
});

describe("credential authority and truthful errors", () => {
  it.each(["purpose", "owner", "namespace"])("rejects conflicting %s without takeover", async variant => {
    const { state, execute, run } = cluster();
    state.source = source(); state.sandbox!.spec.credentialsRef = reference;
    if (variant === "purpose") state.source.metadata.annotations![SOURCE.purpose] = "other";
    if (variant === "owner") state.source.metadata.annotations![SOURCE.sandboxUid] = "old-sandbox";
    if (variant === "namespace") state.source.metadata.namespace = "other-workspace";
    const original = structuredClone(state.source);
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts({ useSource: true }))).rejects.toThrow();
    expect(state.source).toEqual(original);
    expect(run.mock.calls.every(([, args]) => args[0] === "get")).toBe(true);
  });

  it("propagates non404 errors and rejects UID/RV races without exposing request bodies", async () => {
    const { state, execute } = cluster();
    state.failRead = true;
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts())).rejects.toThrow("Read credential resource failed");
    state.failRead = false; state.source = source(); state.sandbox!.spec.credentialsRef = reference;
    state.failWrite = true;
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts({
      updates: { TELEGRAM_BOT_TOKEN: "private-token-value" },
    }))).rejects.not.toThrow("private-token-value");
    state.failWrite = false; state.recreateSourceOnPatch = true;
    await expect(updateCredentialSource(execute, "demo", "kars-system", opts({
      updates: { TELEGRAM_BOT_TOKEN: "new-data" },
    }))).rejects.toThrow("Update credential source failed");
    expect(decode(state.source).TELEGRAM_BOT_TOKEN).toBe("initial-source");
  });

  it("does not report activation when the controller has not acknowledged the source version", async () => {
    const { state, execute } = cluster();
    state.source = source(); state.sandbox!.spec.credentialsRef = reference;
    state.acknowledge = false;
    await expect(waitForCredentialSource(execute, "demo", "kars-system", reference, { attempts: 1, delayMs: 0 }))
      .rejects.toThrow("did not confirm");
    state.sandbox!.spec.credentialsRef = null;
    await expect(verifyCredentialReference(execute, "demo", "kars-system", reference)).rejects.toThrow("did not retain");
  });

  it("allows a repaired source to replace an older failure condition", async () => {
    const { state, execute, run } = cluster();
    state.source = source(); state.sandbox!.spec.credentialsRef = reference;
    state.acknowledge = false;
    state.sandbox!.status = { conditions: [{
      type: "Degraded", status: "True", reason: "CredentialSourceUnavailable", message: "old failure",
    }] };
    const original = run.getMockImplementation()!;
    run.mockImplementation(async (...args) => {
      const result = await original(...args);
      if (args[1].includes("jsonpath={.metadata}")) state.acknowledge = true;
      return result;
    });
    await expect(waitForCredentialSource(execute, "demo", "kars-system", reference, { attempts: 2, delayMs: 0 }))
      .resolves.toBeUndefined();
  });
});
