// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const root = new URL("../../../", import.meta.url);
const source = (path: string) => readFileSync(new URL(path, root), "utf8");

describe("shared Rust security guard coverage", () => {
  it.each(["no-stubs.sh", "no-custom-crypto.sh"])("includes shared code in %s production paths", name => {
    const paths = source(`ci/${name}`).match(/PROD_PATHS=\(([\s\S]*?)\n\)/)?.[1];
    expect(paths).toBeDefined();
    expect(paths).toContain("'shared/'");
  });

  it("requires a capability audit for shared Rust changes without gating Markdown", () => {
    const pattern = source("ci/security-audit-required.sh").match(/^CAP_RE='([^']+)'$/m)?.[1];
    expect(pattern).toBeDefined();
    const result = spawnSync("grep", ["-E", pattern!], {
      encoding: "utf8",
      input: [
        "shared/sre_privacy.rs", "shared/future/authority.rs",
        "shared/README.md", "docs/shared/sre_privacy.rs",
      ].join("\n"),
    });
    expect(result.status).toBe(0);
    expect(result.stdout.trim().split("\n")).toEqual([
      "shared/sre_privacy.rs", "shared/future/authority.rs",
    ]);
  });
});
