// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import { cliVersion, cliReleaseTag } from "./version.js";

describe("cliVersion / cliReleaseTag", () => {
  it("reads a concrete semver from the CLI package.json", () => {
    const v = cliVersion();
    expect(v).toMatch(/^\d+\.\d+\.\d+/);
    expect(v).not.toBe("0.0.0"); // package.json must be resolvable
  });

  it("returns a v-prefixed release tag", () => {
    const tag = cliReleaseTag();
    expect(tag).toMatch(/^v\d+\.\d+\.\d+/);
    expect(tag).toBe(`v${cliVersion()}`);
  });
});
