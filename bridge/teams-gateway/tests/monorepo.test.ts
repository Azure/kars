// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { existsSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { describe, expect, it } from "vitest";

const repository = new URL("../../../", import.meta.url);
const read = (path: string) => readFileSync(new URL(path, repository), "utf8");

describe("optional Bridge monorepo boundary", () => {
  it("includes the whole application without making it a core Cargo member", () => {
    for (const path of [
      "bridge/bff/Cargo.toml", "bridge/bff/Cargo.lock", "bridge/web/package.json",
      "bridge/teams-gateway/package.json", "bridge/deploy/helm/kars-bridge/Chart.yaml",
      "bridge/docs/README.md",
    ]) {
      expect(existsSync(new URL(path, repository)), path).toBe(true);
    }
    const cargo = read("Cargo.toml");
    expect(cargo.match(/members\s*=\s*\[([\s\S]*?)\]/)?.[1]).not.toContain("bridge/");
    expect(cargo.match(/exclude\s*=\s*\[([\s\S]*?)\]/)?.[1]).toContain('"bridge/bff"');
    expect(read("bridge/bff/Cargo.toml")).not.toContain(".workspace = true");
  });

  it("keeps application builds opt-in and source qualification on the same commit", () => {
    const makefile = read("Makefile");
    expect(makefile).toContain(".DEFAULT_GOAL := help");
    expect(makefile.match(/^build:.*$/m)?.[0]).not.toContain("bridge");
    expect(makefile).toContain("$(MAKE) -C bridge check");
    const workflow = read(".github/workflows/bridge-native.yml");
    expect(workflow).toContain("working-directory: bridge");
    expect(workflow).toContain("path: bridge/.native/core");
    expect(workflow).toContain("CORE_REVISION: ${{ github.event.pull_request.head.sha || github.sha }}");
    expect(workflow).not.toMatch(/pallakatos\/|pull_request_target|docker push|freeze-images/);
    expect(read(".github/workflows/bridge-ci.yml")).toContain(
      "node ci/npm-audit-bulk.mjs bridge/${{ matrix.project }}/package-lock.json");
    expect(read("bridge/Makefile")).toContain("\nimage-gateway:\n");
    expect(read("bridge/Makefile").match(/^images:.*$/m)?.[0]).not.toContain("gateway");
  });

  it("includes application production code in the existing source gates", () => {
    for (const gate of ["ci/no-custom-crypto.sh", "ci/no-stubs.sh"]) {
      for (const path of ["bridge/bff/src/", "bridge/web/src/", "bridge/teams-gateway/src/"]) {
        expect(read(gate), `${gate}: ${path}`).toContain(`'${path}'`);
      }
    }
    expect(read("ci/security-audit-required.sh")).toContain(
      "bridge/(bff/src/|web/src/|teams-gateway/src/|deploy/|");
    expect(read("ci/no-null-provider-prod.sh")).toContain("'bridge/deploy/'");
    expect(read(".github/codeql-config.yml")).toContain("paths-ignore: []");
  });

  it("documents public integration without claiming a qualified image release", () => {
    const compatibility = read("bridge/docs/compatibility.md");
    expect(compatibility).toContain("**Azure/kars**");
    expect(compatibility).toContain("**`kars-bridge`**");
    expect(compatibility).toContain("same immutable monorepo commit");
    expect(compatibility).toContain("not yet release-qualified");
    expect(compatibility).not.toContain("prepares a\n**private Bridge PR**");
    expect(read("bridge/docs/deployment.md")).toContain("repository root (`cd bridge`)");
  });

  it("does not include operator state or a cluster-specific deployment overlay", () => {
    expect(existsSync(new URL("bridge/.openclaw/", repository))).toBe(false);
    expect(existsSync(new URL("bridge/deploy/helm/kars-bridge/values-aks-airunway.yaml", repository)))
      .toBe(false);
    expect(read("bridge/start-bff.sh")).not.toMatch(/lsof|kill|nohup/);
    expect(read("bridge/start-bff.sh")).toContain("exec cargo run --locked");
    const tracked = execFileSync("git", ["ls-files", "--stage", "bridge/"], {
      cwd: repository, encoding: "utf8",
    });
    expect(tracked).not.toMatch(/^120000 /m);
    expect(tracked).not.toMatch(/\/(?:node_modules|\.native|\.openclaw|target|dist)(?:\/|$)/m);
    expect(tracked).not.toMatch(/\/\.env(?:\t|$)/m);
  });
});
