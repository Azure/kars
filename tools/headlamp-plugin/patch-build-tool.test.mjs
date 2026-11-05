// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { patchPluginFixtures } from "./patch-build-tool.mjs";
import "./checker/checks.node.mjs";
import "./build-tool/checks.node.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const oldSum = `github.com/rabbitmq/amqp091-go v1.3.4 h1:tXuIslN1nhDqs2t6Jrz3BAoqvt4qIZzxvdbdcxWtHYU=
github.com/rabbitmq/amqp091-go v1.3.4/go.mod h1:ogQDLSOACsLPsIq0NpbtiifNZi2YOz0VTJ0kHRghqbM=
github.com/redis/go-redis/v9 v9.8.0 h1:q3nRvjrlge/6UD7eTu/DSg2uYiU2mCL0G/uzBWqhicI=
`;

function fixture(t) {
  const root = path.join(here, `.build-tool-test-${randomUUID()}`);
  mkdirSync(root);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "@kinvolk/headlamp-plugin", version: "0.13.1" }));
  const modules = ["consume", "publish"].map(name => {
    const directory = path.join(root, "official-plugins/keda/test-files", name);
    mkdirSync(directory, { recursive: true });
    writeFileSync(path.join(directory, "go.mod"), `module github.com/headlamp-k8s/plugins/keda/${name}\n\ngo 1.18\n\nrequire github.com/rabbitmq/amqp091-go v1.3.4\n\nrequire github.com/redis/go-redis/v9 v9.8.0\n`);
    writeFileSync(path.join(directory, "go.sum"), oldSum);
    writeFileSync(path.join(directory, `${name}.go`), 'package main\nimport amqp "github.com/rabbitmq/amqp091-go"\nfunc main() { _, _ = amqp.Dial("amqp://localhost") }\n');
    return directory;
  });
  return { root, modules };
}

function snapshot(f) {
  return f.modules.flatMap(directory => ["go.mod", "go.sum"].map(name => readFileSync(path.join(directory, name), "utf8")));
}

test("patches both real module requirements/checksums, retaining source and unrelated dependencies", t => {
  const f = fixture(t);
  patchPluginFixtures(f.root);
  for (const [i, directory] of f.modules.entries()) {
    const mod = readFileSync(path.join(directory, "go.mod"), "utf8");
    assert(mod.includes("go 1.20\n"));
    assert(mod.includes("require github.com/rabbitmq/amqp091-go v1.14.0\n"));
    assert(mod.includes("require github.com/redis/go-redis/v9 v9.8.0\n"));
    const sum = readFileSync(path.join(directory, "go.sum"), "utf8");
    assert(!sum.includes("v1.3.4"));
    assert(sum.includes("github.com/rabbitmq/amqp091-go v1.14.0 h1:RSaT7aOKt/OrkVUyswPDW29lnRz9psuGmfZFBmLqLek=\n"));
    assert(sum.includes("github.com/rabbitmq/amqp091-go v1.14.0/go.mod h1:Hy4jKW5kQART1u+JkDTF9YYOQUHXqMuhrgxOEeS7G4o=\n"));
    assert(sum.includes("github.com/redis/go-redis/v9 v9.8.0 h1:q3nRvjrlge/6UD7eTu/DSg2uYiU2mCL0G/uzBWqhicI=\n"));
    assert.equal(sum.split("go.uber.org/goleak ").length - 1, 2);
    assert(readFileSync(path.join(directory, `${i ? "publish" : "consume"}.go`), "utf8").includes('amqp.Dial("amqp://localhost")'));
  }
  const prepared = snapshot(f);
  patchPluginFixtures(f.root);
  assert.deepEqual(snapshot(f), prepared);
});

for (const [file, before, after] of [
  ["go.mod", "amqp091-go v1.3.4", "amqp091-go v1.3.5"],
  ["go.sum", "h1:tXuIslN1nhDqs2t6Jrz3BAoqvt4qIZzxvdbdcxWtHYU=", "h1:changed"],
]) {
  test(`rejects upstream ${file} drift without partially modifying the other fixture`, t => {
    const f = fixture(t);
    const changed = path.join(f.modules[1], file);
    writeFileSync(changed, readFileSync(changed, "utf8").replace(before, after));
    const original = snapshot(f);
    assert.throws(() => patchPluginFixtures(f.root), /changed/);
    assert.deepEqual(snapshot(f), original);
  });
}

test("requires review for a different build-tool release", t => {
  const f = fixture(t);
  writeFileSync(path.join(f.root, "package.json"), JSON.stringify({ name: "@kinvolk/headlamp-plugin", version: "0.14.0" }));
  const original = snapshot(f);
  assert.throws(() => patchPluginFixtures(f.root), /Review the fixture patch/);
  assert.deepEqual(snapshot(f), original);
});

test("patch applies to copies of the installed upstream examples without changing their Go source", t => {
  const upstream = path.join(here, "node_modules/@kinvolk/headlamp-plugin");
  if (!existsSync(path.join(upstream, "package.json"))) {
    t.skip("Existing plugin dependencies required; no installation is performed");
    return;
  }
  const f = fixture(t);
  copyFileSync(path.join(upstream, "package.json"), path.join(f.root, "package.json"));
  for (const [i, name] of ["consume", "publish"].entries()) {
    for (const file of ["go.mod", "go.sum", `${name}.go`]) {
      copyFileSync(path.join(upstream, "official-plugins/keda/test-files", name, file), path.join(f.modules[i], file));
    }
  }
  patchPluginFixtures(f.root);
  for (const [i, name] of ["consume", "publish"].entries()) {
    assert.deepEqual(readFileSync(path.join(f.modules[i], `${name}.go`)),
      readFileSync(path.join(upstream, "official-plugins/keda/test-files", name, `${name}.go`)));
    assert(readFileSync(path.join(f.modules[i], "go.mod"), "utf8").includes("amqp091-go v1.14.0"));
  }
  const patched = snapshot(f);
  patchPluginFixtures(f.root);
  assert.deepEqual(snapshot(f), patched);
});

test("npm installs and the image build use the same maintained patch", () => {
  const manifest = JSON.parse(readFileSync(path.join(here, "package.json"), "utf8"));
  const lock = JSON.parse(readFileSync(path.join(here, "package-lock.json"), "utf8"));
  assert.equal(manifest.scripts.postinstall, "node patch-build-tool.mjs");
  assert.equal(manifest.scripts.test, "node --test patch-build-tool.test.mjs && vitest run --config vitest.config.mts");
  assert.equal(lock.packages[""].hasInstallScript, true);
  assert.equal(lock.packages["node_modules/@kinvolk/headlamp-plugin"].version, "0.13.1");
  const docker = readFileSync(path.join(here, "../../sandbox-images/headlamp/Dockerfile"), "utf8");
  assert(docker.includes("COPY tools/headlamp-plugin/package.json tools/headlamp-plugin/package-lock.json tools/headlamp-plugin/patch-build-tool.mjs ./"));
  assert(readFileSync(path.join(here, "../../sandbox-images/headlamp/Dockerfile.dockerignore"), "utf8").includes("!tools/headlamp-plugin/patch-build-tool.mjs"));
});
