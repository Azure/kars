// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { productionPolyfills, verifyProductionLock } from "./build-frontend.mjs";

const command = fileURLToPath(new URL("build-frontend.mjs", import.meta.url));
const upstreamConfig = `import { nodePolyfills } from 'vite-plugin-node-polyfills';
export default {
  define: { global: 'globalThis' },
  envPrefix: 'REACT_APP_',
  plugins: [
    nodePolyfills({
      include: ['process', 'buffer', 'stream'],
    }),
  ],
  build: { outDir: 'build', commonjsOptions: { transformMixedEsModules: true } },
};
`;

function fixture(t, app = { version: "0.41.0", productName: "Headlamp" }) {
  const root = path.join(path.dirname(command), `.frontend-test-${randomUUID()}`);
  mkdirSync(root);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const frontend = path.join(root, "frontend");
  mkdirSync(frontend);
  mkdirSync(path.join(root, "app"));
  writeFileSync(path.join(root, "app/package.json"), JSON.stringify(app));
  const manifest = {
    name: "headlamp-frontend",
    dependencies: {
      "js-yaml": "4.1.0", lodash: "4.17.21", vite: "6.4.3", react: "18.3.1",
      "vite-plugin-node-polyfills": "^0.23.0", "vite-plugin-svgr": "^4.3.0",
      "vite-plugin-static-copy": "^3.1.2", "monaco-editor": "^0.52.0", typescript: "5.6.2",
    },
    devDependencies: {
      "@storybook/react-vite": "^9.1.19", vitest: "^3.0.5",
      "@rsbuild/plugin-node-polyfill": "^1.4.1", "@rsbuild/plugin-svgr": "^1.2.2",
    },
    overrides: { "fast-uri@^3.0.0": "3.1.7", unrelated: "1.2.3" },
    scripts: { build: "vite build", star: "rsbuild dev", storybook: "storybook dev", test: "vitest" },
    optionalDependencies: { "@rollup/rollup-linux-x64-gnu": "*" },
  };
  const manifestPath = path.join(frontend, "package.json");
  writeFileSync(manifestPath, JSON.stringify(manifest));
  writeFileSync(path.join(frontend, "vite.config.ts"), upstreamConfig);
  writeFileSync(path.join(frontend, "rsbuild.config.ts"), "export default {};");
  mkdirSync(path.join(frontend, ".storybook"));
  writeFileSync(path.join(frontend, ".storybook/main.ts"), "export default {};");
  const run = (mode = "prepare") => spawnSync(process.execPath, [command, mode], { cwd: frontend, encoding: "utf8" });
  return { frontend, manifestPath, manifest, run };
}

test("preparation pins patched dependencies while preserving upstream build settings", t => {
  const f = fixture(t);
  const result = f.run();
  assert.equal(result.status, 0, result.stderr);
  const manifest = JSON.parse(readFileSync(f.manifestPath, "utf8"));
  const expected = structuredClone(f.manifest);
  delete expected.devDependencies;
  delete expected.dependencies["vite-plugin-node-polyfills"];
  Object.assign(expected.dependencies, {
    "@rollup/plugin-inject": "5.0.5", buffer: "6.0.3", process: "0.11.10", "stream-browserify": "3.0.0",
    "js-yaml": "4.3.2", lodash: "4.18.1", vite: "6.4.5",
  });
  Object.assign(expected.overrides, { "js-yaml": "$js-yaml", lodash: "$lodash", "fast-uri@^3.0.0": "3.1.9", "nanoid@^3.0.0": "3.3.18" });
  expected.scripts = { build: "node --max-old-space-size=3072 kars-build.mjs build" };
  assert.deepEqual(manifest, expected);
  const configPath = path.join(f.frontend, "vite.config.ts");
  const config = readFileSync(configPath, "utf8");
  assert.equal(config, upstreamConfig
    .replace("import { nodePolyfills } from 'vite-plugin-node-polyfills';\n", "// Node polyfills are supplied by kars-build.mjs for the production build.\n")
    .replace("    nodePolyfills({\n      include: ['process', 'buffer', 'stream'],\n    }),\n", ""));
  for (const removed of ["rsbuild.config.ts", ".storybook/main.ts"]) {
    assert.throws(() => readFileSync(path.join(f.frontend, removed)), { code: "ENOENT" });
  }
  assert.deepEqual(Object.fromEntries(readFileSync(path.join(f.frontend, ".env"), "utf8").trim().split("\n").map(line => line.split("="))), {
    REACT_APP_HEADLAMP_VERSION: "0.41.0",
    REACT_APP_HEADLAMP_GIT_VERSION: "066c5be9deb14b12bcef3485b531c64a84a2a219",
    REACT_APP_HEADLAMP_PRODUCT_NAME: "Headlamp",
    REACT_APP_ENABLE_REACT_QUERY_DEVTOOLS: "false",
    REACT_APP_HEADLAMP_SIDEBAR_DEFAULT_OPEN: "true",
  });
  const prepared = readFileSync(f.manifestPath);
  assert.equal(f.run().status, 0);
  assert.deepEqual(readFileSync(f.manifestPath), prepared);
  assert.equal(readFileSync(configPath, "utf8"), config);
});

for (const config of [
  upstreamConfig.replace("'process', 'buffer', 'stream'", "'process', 'buffer', 'stream', 'crypto'"),
  upstreamConfig + "import { nodePolyfills } from 'vite-plugin-node-polyfills';\n",
]) {
  test("preparation rejects changed upstream polyfill wiring before writing", t => {
    const f = fixture(t);
    writeFileSync(path.join(f.frontend, "vite.config.ts"), config);
    const before = readFileSync(f.manifestPath);
    assert.notEqual(f.run().status, 0);
    assert.deepEqual(readFileSync(f.manifestPath), before);
    assert.equal(readFileSync(path.join(f.frontend, "vite.config.ts"), "utf8"), config);
    assert.throws(() => readFileSync(path.join(f.frontend, ".env")), { code: "ENOENT" });
  });
}

for (const app of [{ version: "0.42.0", productName: "Headlamp" }, { version: "0.41.0", productName: "Changed" }]) {
  test(`preparation rejects unexpected upstream identity ${JSON.stringify(app)} before writing`, t => {
    const f = fixture(t, app);
    const before = readFileSync(f.manifestPath);
    assert.notEqual(f.run().status, 0);
    assert.deepEqual(readFileSync(f.manifestPath), before);
    assert.throws(() => readFileSync(path.join(f.frontend, ".env")), { code: "ENOENT" });
  });
}

test("production lock verification rejects either finding at any depth without filtering other packages", t => {
  const f = fixture(t);
  assert.equal(f.run().status, 0);
  const manifest = JSON.parse(readFileSync(f.manifestPath, "utf8"));
  const lock = { lockfileVersion: 3, packages: {
    "": { dependencies: manifest.dependencies },
    "node_modules/unrelated": { version: "1.2.3" },
  } };
  verifyProductionLock(manifest, lock);
  for (const entry of ["node_modules/deepmerge", "node_modules/parent/node_modules/elliptic", "node_modules/alias"]) {
    const changed = structuredClone(lock);
    changed.packages[entry] = { name: entry.endsWith("alias") ? "elliptic" : undefined, version: "4.3.1" };
    assert.throws(() => verifyProductionLock(manifest, changed), /Unresolved dependency finding/);
  }
  const mismatch = structuredClone(lock);
  mismatch.packages[""].dependencies.react = "0.0.0";
  assert.throws(() => verifyProductionLock(manifest, mismatch), /does not match/);
  const staleDev = structuredClone(lock);
  staleDev.packages[""].devDependencies = { storybook: "9.1.19" };
  assert.throws(() => verifyProductionLock(manifest, staleDev), /Development roots/);
  const before = JSON.stringify(lock);
  writeFileSync(path.join(f.frontend, "package-lock.json"), before);
  assert.equal(f.run("verify-lock").status, 0);
  assert.equal(readFileSync(path.join(f.frontend, "package-lock.json"), "utf8"), before);
  lock.packages["node_modules/elliptic"] = { version: "6.6.1" };
  writeFileSync(path.join(f.frontend, "package-lock.json"), JSON.stringify(lock));
  assert.notEqual(f.run("build").status, 0, "Build must reject the lock before importing build dependencies");
});

test("polyfills retain exact builtin aliases and global injection, never crypto stubs", () => {
  let injected;
  const plugin = productionPolyfills(options => { injected = options; return { name: "test-inject" }; }, name => `/packages/${name}`);
  assert.equal(plugin.apply, "build");
  const config = plugin.config();
  const aliases = Object.fromEntries(config.resolve.alias.map(({ find, replacement }) => [find, replacement]));
  assert.deepEqual(Object.keys(aliases).sort(), ["buffer", "node:buffer", "node:process", "node:stream", "process", "stream"]);
  for (const name of ["buffer", "process", "stream"]) assert.equal(aliases[name], aliases[`node:${name}`]);
  assert.equal(aliases.stream, "/packages/stream-browserify");
  assert.deepEqual(injected.Buffer, [aliases.buffer, "default"]);
  assert.deepEqual(injected.process, [aliases.process, "default"]);
  assert.equal(plugin.load(injected.global[0]), "const global = globalThis; export { global, global as default };");
  for (const id of [aliases.buffer, aliases.process, injected.global[0]]) assert.equal(plugin.resolveId(id), id);
  for (const id of ["crypto", "node:crypto"]) assert.throws(() => plugin.resolveId(id), /real reviewed implementation/);
  assert.equal(plugin.resolveId("other-package"), null);
  assert.equal(plugin.load("crypto"), null);
});

function installedDependencies(t) {
  const require = createRequire(new URL("../../tools/headlamp-plugin/package.json", import.meta.url));
  try {
    for (const name of ["@rollup/plugin-inject", "buffer/", "process/browser.js", "stream-browserify", "acorn"]) require.resolve(name);
  } catch {
    t.skip("Optional integration checks need existing tools/headlamp-plugin dependencies; no installation is performed");
    return null;
  }
  return require;
}

test("real Rollup injection preserves free globals and local bindings", t => {
  const require = installedDependencies(t);
  if (!require) return;
  const plugin = productionPolyfills(require("@rollup/plugin-inject"), require.resolve);
  const inject = plugin.config().build.rollupOptions.plugins[0];
  const context = { parse: code => require("acorn").parse(code, { ecmaVersion: "latest", sourceType: "module" }), warn: message => assert.fail(JSON.stringify(message)) };
  const output = inject.transform.call(context, "export const values = [Buffer.from('a'), process.browser, global];", "/fixture/input.js");
  for (const name of ["Buffer", "process", "global"]) assert(output.code.includes(`import { default as ${name} }`));
  const local = "export function local(Buffer, process, global) { return [Buffer, process, global]; }";
  assert.equal(inject.transform.call(context, local, "/fixture/local.js")?.code ?? local, local);
});

test("real package shims preserve Buffer and process default/named exports", async t => {
  const require = installedDependencies(t);
  if (!require) return;
  const f = fixture(t);
  const plugin = productionPolyfills(require("@rollup/plugin-inject"), require.resolve);
  const aliases = Object.fromEntries(plugin.config().resolve.alias.map(({ find, replacement }) => [find, replacement]));
  const load = async name => {
    const file = path.join(f.frontend, `${name}.mjs`);
    writeFileSync(file, plugin.load(aliases[name]));
    return import(pathToFileURL(file).href);
  };
  const buffer = await load("buffer");
  assert.equal(buffer.default, buffer.Buffer);
  assert.equal(buffer.Buffer.from("Headlamp").toString("base64"), "SGVhZGxhbXA=");
  const process = await load("process");
  assert.equal(process.default, process.process);
  assert.equal(process.process.browser, true);
  await new Promise(resolve => process.process.nextTick(resolve));
  const { PassThrough } = require("stream-browserify");
  const stream = new PassThrough();
  const chunks = [];
  stream.on("data", chunk => chunks.push(chunk));
  await new Promise((resolve, reject) => { stream.on("end", resolve); stream.on("error", reject); stream.end(buffer.Buffer.from("streamed")); });
  assert.equal(buffer.Buffer.concat(chunks).toString(), "streamed");
});
