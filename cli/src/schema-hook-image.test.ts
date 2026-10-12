// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";

it("limits the schema toolchain context to its recipe and build script without reopening parent directories", () => {
  const patterns = readFileSync(new URL("../schema-hook/Dockerfile.dockerignore", import.meta.url), "utf8")
    .split(/\r?\n/)
    .map(line => line.trim())
    .filter(line => line.length > 0 && !line.startsWith("#"));

  expect(patterns).toEqual([
    "**",
    "!cli/schema-hook/Dockerfile",
    "!cli/schema-hook/Dockerfile.dockerignore",
    "!cli/schema-hook/build-tools.sh",
  ]);
});

it("builds pinned upstream clients with patched dependencies without adding tools to the runtime", () => {
  const recipe = readFileSync(new URL("../schema-hook/Dockerfile", import.meta.url), "utf8");
  const script = readFileSync(new URL("../schema-hook/build-tools.sh", import.meta.url), "utf8");
  expect(recipe).toContain("FROM golang:1.27.2-bookworm AS tools");
  expect(recipe.match(/^ADD --checksum=sha256:[a-f0-9]{64} https:\/\/codeload\.github\.com\/[^ ]+\/[a-f0-9]{40} /gm))
    .toHaveLength(2);
  expect(recipe).not.toMatch(/HELM_SHA256|KUBECTL_SHA256|get\.helm\.sh|dl\.k8s\.io/);
  expect(script.match(/go mod edit -require=golang\.org\/x\/net@v0\.60\.0/g)).toHaveLength(2);
  expect(script).toContain("version.version=v4.3.0");
  expect(script).toContain("version.metadata=kars.1");
  expect(script).toContain("version.gitVersion=v1.35.9+kars.1");
  expect(script.match(/version\.gitTreeState=dirty/g)).toHaveLength(2);
  expect(script).toContain("go test -mod=mod -p=2 ./internal/version");
  expect(script).toContain("go test -mod=mod -p=2 k8s.io/kubectl/pkg/cmd/version");
  const runtime = recipe.split("FROM runtime-base AS runtime\n")[1];
  expect(runtime).toBeDefined();
  expect(runtime).toContain("USER 10001:10001");
  expect(runtime).toContain('ENTRYPOINT ["node"]');
  expect(runtime).not.toMatch(/build-schema-tools|go build|go mod|\/usr\/local\/go/);
});

it("uses a valid POSIX shell build entrypoint", () => {
  expect(() => execFileSync("sh", ["-n",
    fileURLToPath(new URL("../schema-hook/build-tools.sh", import.meta.url))])).not.toThrow();
});
