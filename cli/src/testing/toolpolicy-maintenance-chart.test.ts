// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";

const chart = fileURLToPath(new URL("../../../deploy/helm/kars/", import.meta.url));
function render(replicas: number, fenced: boolean, upgrade: boolean) {
  const directory = mkdtempSync(join(tmpdir(), "kars-toolpolicy-maintenance-"));
  try {
    mkdirSync(join(directory, "templates"));
    mkdirSync(join(directory, "files"));
    writeFileSync(join(directory, "Chart.yaml"), "apiVersion: v2\nname: policy-check\nversion: 0.1.0\n");
    copyFileSync(join(chart, "templates/toolpolicy-default.yaml"), join(directory, "templates/policy.yaml"));
    copyFileSync(join(chart, "files/kars-default-agt-profile.yaml"), join(directory, "files/kars-default-agt-profile.yaml"));
    writeFileSync(join(directory, "values.yaml"), JSON.stringify({ controller: { replicas,
      ...(fenced ? { upgradePrecondition: { uid: "controller-uid", resourceVersion: "123" } } : {}) } }));
    return parseAllDocuments(execFileSync("helm", ["template", "kars", directory, "--namespace", "kars-system",
      ...(upgrade ? ["--is-upgrade"] : [])], { encoding: "utf8", timeout: 10000, stdio: ["ignore", "pipe", "pipe"] }))
      .map(document => document.toJSON()).filter(Boolean);
  } finally { rmSync(directory, { recursive: true, force: true }); }
}

describe("default ToolPolicy during controller maintenance", () => {
  it("does not delete/recreate a finalizer-protected policy while a fenced Core is paused", () => {
    expect(render(0, true, true)).toEqual([]);
  });
  it.each([[1, true, true], [1, false, true], [0, false, true], [0, true, false], [1, false, false]])(
    "retains normal publication with replicas=%s fenced=%s upgrade=%s", (replicas, fenced, upgrade) => {
      const [policy] = render(replicas as number, fenced as boolean, upgrade as boolean);
      expect(policy.kind).toBe("ToolPolicy");
      expect(policy.metadata.name).toBe("kars-default");
      expect(policy.metadata.annotations["helm.sh/hook"]).toBe("post-install,post-upgrade");
      expect(policy.spec.agtProfile.inline).toContain("kars-default");
    });
});
