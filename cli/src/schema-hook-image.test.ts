// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { expect, it } from "vitest";

it("limits the schema toolchain context to its two recipe files without reopening parent directories", () => {
  const patterns = readFileSync(new URL("../schema-hook/Dockerfile.dockerignore", import.meta.url), "utf8")
    .split(/\r?\n/)
    .map(line => line.trim())
    .filter(line => line.length > 0 && !line.startsWith("#"));

  expect(patterns).toEqual([
    "**",
    "!cli/schema-hook/Dockerfile",
    "!cli/schema-hook/Dockerfile.dockerignore",
  ]);
});
