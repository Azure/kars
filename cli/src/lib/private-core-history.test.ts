// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { canonical, type Execute } from "./private-activation.js";
import { assertHistoricalHooksAbsent, freezeHistoricalHookFailure, historicalHookProof, HISTORICAL_HOOK_RETIREMENT,
  verifyHistoricalHookRetirement } from "./private-core-history.js";
import { historicalRelease } from "./private-core-history.test-support.js";

const fixture = () => {
  const history = [historicalRelease(1, "superseded"), historicalRelease(2, "failed"), historicalRelease(3, "deployed")];
  history[1]!.release.hooks[6].last_run.phase = "";
  return { history, failed: history[1]! };
};
const absent = vi.fn<Execute>(async args => args[1] === "pods" ? JSON.stringify({ metadata: {}, items: [] }) : "");

describe("historical admission hook retirement", () => {
  it("binds exact failed bytes and successful successor intent without changing failure status", () => {
    const { failed, history } = fixture();
    const before = canonical(history), proof = historicalHookProof(failed, history);
    expect(proof.failed).toMatchObject({ revision: 2, uid: failed.uid, digest: failed.digest });
    expect(proof.successor.revision).toBe(3);
    expect(proof.resources).toHaveLength(7);
    expect(canonical(history)).toBe(before);
  });

  it.each(["empty", "latest", "pending", "csa", "gap", "unknown", "changed", "unfinished", "api", "account", "binding", "credentials"])(
    "rejects unsupported %s evidence", mode => {
      const { failed, history } = fixture();
      if (mode === "empty") history.length = 0;
      if (mode === "latest") history.pop();
      if (mode === "pending") failed.release.info.status = "pending-upgrade";
      if (mode === "csa") failed.release.apply_method = "csa";
      if (mode === "gap") history[0]!.revision = 9;
      if (mode === "unknown") failed.release.hooks[0].path += ".unknown";
      if (mode === "changed") history[2]!.release.hooks[0].manifest += "\n# changed";
      if (mode === "unfinished") history[2]!.release.hooks[0].last_run.phase = "Running";
      if (["api", "account", "binding", "credentials"].includes(mode)) {
        const index = mode === "binding" ? 2 : mode === "api" ? 1 : 5;
        for (const entry of [failed, history[2]!]) {
          const hook = entry.release.hooks[index], manifest = JSON.parse(hook.manifest);
          if (mode === "api") manifest.apiVersion = "unreviewed/v1";
          if (mode === "account") manifest.spec.template.spec.serviceAccountName = "another";
          if (mode === "binding") manifest.subjects[0].name = "another";
          if (mode === "credentials") manifest.spec.template.spec.containers[0].envFrom = [{ secretRef: { name: "independent" } }];
          hook.manifest = JSON.stringify(manifest);
        }
      }
      expect(() => historicalHookProof(failed, history)).toThrow();
    });

  it.each(["Job", "ServiceAccount", "Role", "RoleBinding", "ClusterRole", "ClusterRoleBinding", "pod", "invalid-pod", "page", "denied"])(
    "refuses live or unobservable %s authority", async mode => {
      const { failed, history } = fixture(), proof = historicalHookProof(failed, history);
      const run: Execute = async args => {
        if (mode === "denied") throw new Error("Forbidden");
        if (args[1] === mode) return "{}";
        if (args[1] === "pods") return JSON.stringify({ metadata: mode === "page" ? { continue: "more" } : {},
          items: mode === "pod" ? [{ spec: { serviceAccountName: proof.account }, status: { phase: "Succeeded" } }]
            : mode === "invalid-pod" ? [{}] : [] });
        return "";
      };
      await expect(assertHistoricalHooksAbsent(run, proof)).rejects.toThrow();
    });

  it("CAS freezes the complete unchanged failure, preserves annotations and rechecks absence", async () => {
    const { failed, history } = fixture(), proof = historicalHookProof(failed, history);
    failed.object.metadata.annotations = { retained: "evidence" };
    const run = vi.fn<Execute>(async (args, input) => {
      if (args[0] !== "patch") return absent(args, input);
      const patch = JSON.parse(input!);
      expect(patch.slice(0, 2)).toEqual([{ op: "test", path: "/metadata/uid", value: failed.uid },
        { op: "test", path: "/metadata/resourceVersion", value: "1" }]);
      expect(patch.some((op: any) => op.path.startsWith("/data"))).toBe(false);
      return JSON.stringify({ ...failed.object, immutable: true, metadata: { ...failed.object.metadata,
        resourceVersion: "2", annotations: patch[2].value } });
    });
    await freezeHistoricalHookFailure(run, failed, proof);
    expect(run.mock.calls.filter(([args]) => args[1] === "pods")).toHaveLength(2);
    const patch = JSON.parse(run.mock.calls.find(([args]) => args[0] === "patch")![1]!);
    expect(patch[2].value.retained).toBe("evidence");
    expect(failed.object.data.release).toBe("original-failure-bytes");
  });

  it.each(["immutable", "data", "uid", "version", "marker", "lost-ack"])("rejects invalid %s freeze acknowledgement", async mode => {
    const { failed, history } = fixture(), proof = historicalHookProof(failed, history);
    const run: Execute = async (args, input) => {
      if (args[0] !== "patch") return absent(args, input);
      if (mode === "lost-ack") throw new Error("lost acknowledgement");
      const result = structuredClone(failed.object);
      result.immutable = mode !== "immutable";
      result.metadata.resourceVersion = mode === "version" ? "1" : "2";
      result.metadata.annotations = { [HISTORICAL_HOOK_RETIREMENT]: mode === "marker" ? "wrong" : canonical(proof) };
      if (mode === "data") result.data.release = "changed";
      if (mode === "uid") result.metadata.uid = "replacement";
      return JSON.stringify(result);
    };
    await expect(freezeHistoricalHookFailure(run, failed, proof)).rejects.toThrow();
  });

  it("revalidates persistent proof independently of a later migration journal", async () => {
    const { failed, history } = fixture(), proof = historicalHookProof(failed, history);
    await expect(verifyHistoricalHookRetirement(absent, failed, history)).resolves.toBe(false);
    failed.object.immutable = true;
    failed.object.metadata.annotations = { [HISTORICAL_HOOK_RETIREMENT]: canonical(proof) };
    await expect(verifyHistoricalHookRetirement(absent, failed, history)).resolves.toBe(true);
    history[2]!.uid = "replacement-successor";
    await expect(verifyHistoricalHookRetirement(absent, failed, history)).rejects.toThrow();
  });
});
