// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { inventory, prepare } from "./build-frontend.mjs";

function fixture(t, version = "3.11.3") {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "kars-prometheus-ui-"));
  t.after(() => fs.rmSync(directory, { recursive: true }));
  const root = path.join(directory, "web/ui");
  fs.mkdirSync(path.join(root, "react-app"), { recursive: true });
  fs.mkdirSync(path.join(root, "mantine-ui"));
  fs.writeFileSync(path.join(directory, "VERSION"), version + "\n");
  for (const [file, manifest] of [
    ["package.json", { name: "prometheus-io", version: "0.311.3", overrides: { existing: "1.0.0" } }],
    ["react-app/package.json", { name: "@prometheus-io/app", version: "0.300.1", dependencies: { react: "^17.0.2" } }],
    ["mantine-ui/package.json", { name: "@prometheus-io/mantine-ui", version: "0.311.3",
      dependencies: { "react-router-dom": "^7.13.2", react: "^19.2.4" }, devDependencies: { postcss: "^8.5.8" } }],
  ]) fs.writeFileSync(path.join(root, file), JSON.stringify(manifest));
  return root;
}

test("security pins retain both application versions and unrelated dependencies", t => {
  const root = fixture(t);
  prepare(root);
  for (const file of ["package.json", "react-app/package.json"]) {
    const manifest = JSON.parse(fs.readFileSync(path.join(root, file)));
    assert.equal(manifest.overrides["nanoid@^3.0.0"], "3.3.18");
    assert.equal(manifest.overrides["postcss@^8.0.0"], "8.5.18");
  }
  const current = JSON.parse(fs.readFileSync(path.join(root, "mantine-ui/package.json")));
  assert.equal(current.version, "0.311.3");
  assert.equal(current.dependencies["react-router-dom"], "7.18.2");
  assert.equal(current.devDependencies.postcss, "8.5.18");
  assert.equal(current.dependencies.react, "^19.2.4");
  assert.equal(JSON.parse(fs.readFileSync(path.join(root, "package.json"))).overrides.existing, "1.0.0");
  assert.equal(JSON.parse(fs.readFileSync(path.join(root, "react-app/package.json"))).dependencies.react, "^17.0.2");
});

test("pins do not silently cross major versions of older build dependencies", t => {
  const root = fixture(t);
  prepare(root);
  const overrides = JSON.parse(fs.readFileSync(path.join(root, "package.json"))).overrides;
  assert.equal(overrides.postcss, undefined);
  assert.equal(overrides.nanoid, undefined);
  assert.equal(overrides["react-router@^7.0.0"], "7.18.2");
  assert.equal(overrides["react-router-dom@^7.0.0"], "7.18.2");
  assert.equal(overrides["react-router-dom"], undefined);
  const legacy = JSON.parse(fs.readFileSync(path.join(root, "react-app/package.json")));
  assert.equal(legacy.overrides["react-router-dom@^7.0.0"], undefined);
});

test("prepare is idempotent", t => {
  const root = fixture(t);
  prepare(root);
  const before = fs.readFileSync(path.join(root, "package.json"), "utf8");
  prepare(root);
  assert.equal(fs.readFileSync(path.join(root, "package.json"), "utf8"), before);
});

test("a different Prometheus release is rejected before writing", t => {
  const root = fixture(t, "3.12.0");
  const before = fs.readFileSync(path.join(root, "package.json"), "utf8");
  assert.throws(() => prepare(root), /3.11.3/);
  assert.equal(fs.readFileSync(path.join(root, "package.json"), "utf8"), before);
});

function legacyBundle(t, postcss = "8.5.18") {
  const root = fixture(t);
  const legacy = path.join(root, "react-app");
  const build = path.join(legacy, "build");
  fs.mkdirSync(build);
  fs.writeFileSync(path.join(build, "main.js"), "export const main = 1;\n");
  fs.writeFileSync(path.join(build, "bundle-stats.json"), JSON.stringify({ assets: [] }));
  for (const [name, version, extension] of [["postcss", postcss, "js"], ["bootstrap", "4.6.2", "css"]]) {
    const directory = path.join(legacy, "node_modules", name);
    fs.mkdirSync(directory, { recursive: true });
    fs.writeFileSync(path.join(directory, "package.json"), JSON.stringify({ name, version }));
    fs.writeFileSync(path.join(build, `main.${extension}.map`), JSON.stringify({
      version: 3, sources: [`../node_modules/${name}/src/index.${extension}`], sourcesContent: ["source"],
    }));
  }
  return root;
}

test("legacy inventory separates executable packages from CSS-only dependencies", async t => {
  const root = legacyBundle(t);
  await inventory(root, "legacy");
  const sbom = JSON.parse(fs.readFileSync(path.join(root, "legacy.cdx.json")));
  const evidence = JSON.parse(fs.readFileSync(path.join(root, "legacy-evidence.json")));
  assert.deepEqual(sbom.components.map(component => component.name), ["@prometheus-io/app", "postcss"]);
  assert.deepEqual(evidence.cssOnlyPackages, [{ name: "bootstrap", version: "4.6.2" }]);
  assert.match(evidence.files["main.js"], /^[a-f0-9]{64}$/);
});

test("legacy inventory rejects an unfixed rendered dependency", async t => {
  await assert.rejects(inventory(legacyBundle(t, "8.4.38"), "legacy"), /8.5.18/);
});

test("legacy inventory rejects unmapped packages rather than omitting them", async t => {
  const root = legacyBundle(t);
  fs.unlinkSync(path.join(root, "react-app/node_modules/postcss/package.json"));
  await assert.rejects(inventory(root, "legacy"), /Unattributed browser module/);
});

test("embedded helper attribution requires matching the owning package's published source map", async t => {
  const root = legacyBundle(t);
  const owner = path.join(root, "react-app/node_modules/resize");
  fs.mkdirSync(path.join(owner, "build"), { recursive: true });
  fs.writeFileSync(path.join(owner, "package.json"), JSON.stringify({ name: "resize", version: "1.0.0" }));
  fs.writeFileSync(path.join(owner, "build/index.js.map"), JSON.stringify({
    version: 3, sources: ["../node_modules/tslib/index.js"], sourcesContent: ["bundled helper"],
  }));
  const finalMap = path.join(root, "react-app/build/helper.js.map");
  fs.writeFileSync(finalMap, JSON.stringify({
    version: 3, sources: ["../node_modules/resize/node_modules/tslib/index.js"], sourcesContent: ["bundled helper"],
  }));
  await inventory(root, "legacy");
  const evidence = JSON.parse(fs.readFileSync(path.join(root, "legacy-evidence.json")));
  assert.equal(evidence.modules.find(module => module.bundledSource).package, "resize@1.0.0");
  fs.writeFileSync(finalMap, JSON.stringify({
    version: 3, sources: ["../node_modules/resize/node_modules/tslib/index.js"], sourcesContent: ["different code"],
  }));
  fs.copyFileSync(path.join(root, "legacy-webpack-stats.json"), path.join(root, "react-app/build/bundle-stats.json"));
  await assert.rejects(inventory(root, "legacy"), /Unattributed browser module/);
});

test("static CommonJS attribution uses compiler provenance, not a guessed dependency", async t => {
  const root = legacyBundle(t);
  const dependency = path.join(root, "react-app/node_modules/nanoid");
  fs.mkdirSync(path.join(dependency, "non-secure"), { recursive: true });
  fs.writeFileSync(path.join(dependency, "package.json"), JSON.stringify({ name: "nanoid", version: "3.3.18" }));
  fs.writeFileSync(path.join(dependency, "non-secure/index.cjs"), "module.exports = { example: true };");
  const build = path.join(root, "react-app/build");
  fs.writeFileSync(path.join(build, "index.hash.cjs"), "module.exports={example:!0};");
  fs.writeFileSync(path.join(build, "bundle-stats.json"), JSON.stringify({
    assets: [{ name: "index.hash.cjs", info: { sourceFilename: "node_modules/nanoid/non-secure/index.cjs" } }],
  }));
  await inventory(root, "legacy");
  const evidence = JSON.parse(fs.readFileSync(path.join(root, "legacy-evidence.json")));
  assert.equal(evidence.modules.find(module => module.staticAsset).package, "nanoid@3.3.18");
  assert.equal(evidence.files["bundle-stats.json"], undefined);
  assert.equal(fs.existsSync(path.join(build, "bundle-stats.json")), false);
});
