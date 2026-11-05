// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const source = readFileSync(new URL("../../scripts/bundle-deploy-assets.mjs", import.meta.url), "utf8");
const versionLine = source.split("\n").find(line => line.includes('typeof item.version === "string"'))!;
const version = new RegExp(versionLine.slice(versionLine.indexOf("/") + 1, versionLine.lastIndexOf("/")));

describe("bundled dependency versions", () => {
  it.each(["1.2.3", "0.0.0-alpha.1", "1.2.3+build.42", "1.2.3-alpha+build", "1.2.3-a--b", "1.2.3+a+b"])("retains accepted version %s", value => {
    expect(version.test(value)).toBe(true);
  });
  it.each(["1.2", "v1.2.3", "1.2.3+", "1.2.3-", "1.2.3+a++b", "1.2.3/../chart", "1.2.3+metadata!"])("rejects invalid version %s", value => {
    expect(version.test(value)).toBe(false);
  });
  it("rejects long failing suffixes without overlapping repetition", () => {
    expect(version.test(`0.0.0+${"--".repeat(32768)}!`)).toBe(false);
    expect(version.test(`0.0.0+${"a+".repeat(32768)}!`)).toBe(false);
  });
});
