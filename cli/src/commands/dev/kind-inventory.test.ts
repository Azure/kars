// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { listKindClusters } from "./kind-inventory.js";
import { assertFreshKindBetaCluster, type BetaExecute } from "./kind-beta.js";

const tools = { kind: "/tools/kind", runtime: "/tools/podman", kubectl: "/tools/kubectl", helm: "/tools/helm",
  env: { KIND_EXPERIMENTAL_PROVIDER: "podman", CONTAINER_HOST: "unix:///pinned/socket" } };
const stopped = { State: "exited", Labels: { "io.x-k8s.kind.cluster": "old-workspace" } };
describe("Podman Kind inventory", () => {
  it("includes stopped nodes using pinned JSON inventory, never the broken Kind labels template", async () => {
    const execute = vi.fn<BetaExecute>(async () => ({ stdout: JSON.stringify([stopped, stopped]) }));
    expect(await listKindClusters(tools, execute)).toEqual(["old-workspace"]);
    expect(execute).toHaveBeenCalledExactlyOnceWith(tools.runtime, ["ps", "-a", "--filter", "label=io.x-k8s.kind.cluster", "--format", "json"], { env: tools.env, stdio: "pipe" });
    await expect(assertFreshKindBetaCluster("old-workspace", tools, execute)).rejects.toThrow("already exists (including stopped nodes)");
    await expect(assertFreshKindBetaCluster("fresh-workspace", tools, execute)).resolves.toBeUndefined();
  });
  it("accepts an empty valid inventory, not an empty command response", async () => {
    await expect(listKindClusters(tools, async () => ({ stdout: "[]" }))).resolves.toEqual([]);
    await expect(listKindClusters(tools, async () => ({ stdout: "" }))).rejects.toThrow("absence was not established");
  });
  it.each([{}, [null], [{}], [{ Labels: [] }], [{ Labels: {} }], [{ Labels: { "io.x-k8s.kind.cluster": 2 } }],
    [{ Labels: { "io.x-k8s.kind.cluster": "../escape" } }]])("fails closed on malformed inventory %j", async data => {
    await expect(listKindClusters(tools, async () => ({ stdout: JSON.stringify(data) }))).rejects.toThrow("absence was not established");
  });
  it("redacts failed command output", async () => {
    const error = await listKindClusters(tools, async () => { throw new Error("private-runtime-output"); }).catch(error => error);
    expect(error.message).toContain("podman ps -a --format json");
    expect(error.message).not.toContain("private-runtime-output");
  });
});
