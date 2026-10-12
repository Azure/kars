// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

const read = file => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, JSON.stringify(value, null, 2) + "\n");
const hash = file => createHash("sha256").update(fs.readFileSync(file)).digest("hex");

export function prepare(root) {
  assert.equal(fs.readFileSync(path.resolve(root, "../../VERSION"), "utf8").trim(), "3.11.3");
  for (const [file, name, version] of [
    ["package.json", "prometheus-io", "0.311.3"],
    ["react-app/package.json", "@prometheus-io/app", "0.300.1"],
  ]) {
    const manifest = read(path.join(root, file));
    assert.equal(manifest.name, name);
    assert.equal(manifest.version, version);
    manifest.overrides = {
      ...manifest.overrides,
      "nanoid@^3.0.0": "3.3.18",
      "postcss@^8.0.0": "8.5.18",
    };
    if (name === "prometheus-io") {
      Object.assign(manifest.overrides, {
        "react-router@^7.0.0": "7.18.2",
        "react-router-dom@^7.0.0": "7.18.2",
      });
    }
    write(path.join(root, file), manifest);
  }
  const file = path.join(root, "mantine-ui/package.json");
  const manifest = read(file);
  assert.equal(manifest.name, "@prometheus-io/mantine-ui");
  assert.equal(manifest.version, "0.311.3");
  manifest.dependencies["react-router-dom"] = "7.18.2";
  manifest.devDependencies.postcss = "8.5.18";
  write(file, manifest);
}

export async function inventory(root, kind) {
  assert(["legacy", "mantine"].includes(kind));
  const packages = new Map();
  const modules = [];
  const cssPackages = new Map();
  const packageFor = (file, sourceContent) => {
    let directory = path.dirname(file.split("?")[0]);
    const nodeModules = file.lastIndexOf(`${path.sep}node_modules${path.sep}`);
    const boundary = nodeModules === -1 ? root : file.slice(0, nodeModules) + `${path.sep}node_modules`;
    while (directory.startsWith(boundary + path.sep)) {
      const file = path.join(directory, "package.json");
      if (fs.existsSync(file)) {
        const manifest = read(file);
        if (manifest.name && manifest.version) {
          return { name: manifest.name, version: manifest.version };
        }
      }
      directory = path.dirname(directory);
    }
    const owner = file.slice(0, nodeModules);
    if (owner.includes(`${path.sep}node_modules${path.sep}`) && typeof sourceContent === "string"
      && fs.existsSync(path.join(owner, "package.json"))) {
      const containsSource = directory => {
        for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
          const candidate = path.join(directory, entry.name);
          if (entry.isDirectory() && entry.name !== "node_modules" && containsSource(candidate)) return true;
          if (!entry.isFile() || !entry.name.endsWith(".map")) continue;
          const map = read(candidate);
          if (map.sources?.some((source, index) =>
            path.resolve(directory, source) === file && map.sourcesContent?.[index] === sourceContent)) return true;
        }
        return false;
      };
      // Published libraries can embed helpers whose original nested dependency paths no longer exist.
      if (containsSource(owner)) {
        const manifest = read(path.join(owner, "package.json"));
        assert(manifest.name && manifest.version);
        return { name: manifest.name, version: manifest.version, bundledSource: true };
      }
    }
    throw new Error(`Unattributed browser module: ${file}`);
  };
  const record = (file, target, chunk, sourceContent) => {
    const identity = packageFor(file, sourceContent);
    const key = `${identity.name}@${identity.version}`;
    target.set(key, { name: identity.name, version: identity.version });
    const entry = { chunk, module: path.relative(root, file), package: key,
      kind: target === cssPackages ? "css" : "javascript",
      ...(identity.bundledSource ? { bundledSource: true,
        sourceSha256: createHash("sha256").update(sourceContent).digest("hex") } : {}) };
    modules.push(entry);
    return entry;
  };
  const dist = path.join(root, kind === "legacy" ? "react-app/build" : "mantine-ui/dist");
  const stats = kind === "legacy" ? read(path.join(dist, "bundle-stats.json")) : undefined;
  if (kind === "mantine") {
    const { build } = await import(pathToFileURL(path.join(root, "node_modules/vite/dist/node/index.js")));
    await build({
      root: path.join(root, "mantine-ui"),
      plugins: [{
        name: "kars-bundle-evidence",
        generateBundle(_options, bundle) {
          for (const chunk of Object.values(bundle)) {
            if (chunk.type !== "chunk") continue;
            for (const [id, module] of Object.entries(chunk.modules)) {
              if (module.renderedLength && id.startsWith(root + path.sep)) record(id, packages, chunk.fileName);
            }
          }
        },
      }],
    });
  }
  const files = {};
  const visit = directory => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        visit(file);
        continue;
      }
      assert(entry.isFile(), `Unexpected frontend output: ${file}`);
      const relative = path.relative(dist, file);
      if (relative === "bundle-stats.json") continue;
      files[relative] = hash(file);
      if (kind !== "legacy") continue;
      if (file.endsWith(".js.map") || file.endsWith(".css.map")) {
        const map = read(file);
        assert.equal(map.version, 3);
        assert.equal(map.sources.length, map.sourcesContent.length);
        const target = file.endsWith(".css.map") ? cssPackages : packages;
        for (const [index, source] of map.sources.entries()) {
          if (!source.includes("node_modules/")) continue;
          const module = path.resolve(root, "react-app", source.slice(source.indexOf("node_modules/")));
          assert(module.startsWith(path.join(root, "react-app/node_modules") + path.sep));
          record(module, target, relative, map.sourcesContent[index]);
        }
      } else if (file.endsWith(".cjs")) {
        const assets = stats.assets.filter(asset => asset.name === relative);
        assert.equal(assets.length, 1, `Unattributed static CommonJS asset: ${relative}`);
        assert.equal(typeof assets[0].info.sourceFilename, "string");
        const source = path.resolve(root, "react-app", assets[0].info.sourceFilename);
        assert(source.startsWith(root + path.sep));
        Object.assign(record(source, packages, relative), { staticAsset: true, sourceSha256: hash(source) });
      }
    }
  };
  visit(dist);
  assert(Object.keys(files).some(file => file.endsWith(".js")));
  assert(modules.length > 0 && packages.size > 0);
  if (kind === "legacy") {
    const manifest = read(path.join(root, "react-app/package.json"));
    packages.set(`${manifest.name}@${manifest.version}`, { name: manifest.name, version: manifest.version });
  }
  for (const identity of packages.values()) {
    if (identity.name === "postcss") assert.equal(identity.version, "8.5.18");
    if (identity.name === "nanoid" && identity.version.startsWith("3.")) assert.equal(identity.version, "3.3.18");
    if (identity.name === "react-router" && kind === "mantine") assert.equal(identity.version, "7.18.2");
  }
  const components = [...packages.values()]
    .sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`))
    .map(({ name, version }) => ({
      type: "library", name, version, "bom-ref": `${name}@${version}`,
      purl: `pkg:npm/${name.replace(/^@/, "%40")}@${version}`,
    }));
  write(path.join(root, `${kind}-evidence.json`), {
    sourceCommit: "eb173f5256d4022afba1e9bc3d19740a76859fae",
    prometheusVersion: "3.11.3", modules, files,
    cssOnlyPackages: [...cssPackages.entries()].filter(([key]) => !packages.has(key)).map(([, value]) => value),
  });
  write(path.join(root, `${kind}.cdx.json`), { bomFormat: "CycloneDX", specVersion: "1.6", version: 1, components });
  if (kind === "legacy") fs.renameSync(path.join(dist, "bundle-stats.json"), path.join(root, "legacy-webpack-stats.json"));
  console.log(JSON.stringify({ kind, renderedPackages: components.length, modules: modules.length, outputFiles: Object.keys(files).length }));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [mode, kind] = process.argv.slice(2);
  if (mode === "prepare") prepare(process.cwd());
  else if (mode === "inventory") await inventory(process.cwd(), kind);
  else throw new Error("Expected prepare or inventory {legacy|mantine}");
}
