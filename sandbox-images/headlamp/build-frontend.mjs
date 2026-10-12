// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const root = process.cwd();
const read = file => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, JSON.stringify(value, null, 2) + "\n");

if (process.argv[2] === "prepare") {
  const app = read("../app/package.json");
  assert.equal(app.version, "0.41.0");
  assert.equal(app.productName, "Headlamp");
  const manifest = read("package.json");
  Object.assign(manifest.dependencies, {
    "js-yaml": "4.3.2",
    lodash: "4.18.1",
    vite: "6.4.3",
  });
  Object.assign(manifest.overrides, {
    "js-yaml": "$js-yaml",
    lodash: "$lodash",
    "fast-uri@^3.0.0": "3.1.7",
    "nanoid@^3.0.0": "3.3.18",
  });
  write("package.json", manifest);
  const environment = {
    REACT_APP_HEADLAMP_VERSION: app.version,
    REACT_APP_HEADLAMP_GIT_VERSION: "066c5be9deb14b12bcef3485b531c64a84a2a219",
    REACT_APP_HEADLAMP_PRODUCT_NAME: app.productName,
    REACT_APP_ENABLE_REACT_QUERY_DEVTOOLS: "false",
    REACT_APP_HEADLAMP_SIDEBAR_DEFAULT_OPEN: "true",
  };
  fs.writeFileSync(".env", Object.entries(environment).map(([key, value]) => `${key}=${value}\n`).join(""));
} else if (process.argv[2] === "build") {
  const { build } = await import("vite");
  const packages = new Map();
  const modules = [];
  const recordPackage = directory => {
    const manifest = read(path.join(directory, "package.json"));
    assert.equal(typeof manifest.name, "string");
    assert.equal(typeof manifest.version, "string");
    const key = `${manifest.name}@${manifest.version}`;
    packages.set(key, { name: manifest.name, version: manifest.version });
    return key;
  };
  process.env.PUBLIC_URL = "./";
  await build({
    plugins: [{
      name: "kars-bundle-evidence",
      generateBundle(_options, bundle) {
        for (const chunk of Object.values(bundle)) {
          if (chunk.type !== "chunk") continue;
          for (const [id, module] of Object.entries(chunk.modules)) {
            if (!module.renderedLength || !id.startsWith(root + "/node_modules/")) continue;
            let directory = path.dirname(id.split("?")[0]);
            while (directory.startsWith(root + "/node_modules/")) {
              const manifest = path.join(directory, "package.json");
              // Some packages include nested ESM configuration without package identity.
              if (fs.existsSync(manifest) && read(manifest).name && read(manifest).version) break;
              directory = path.dirname(directory);
            }
            assert(directory.startsWith(root + "/node_modules/"), `Unattributed module: ${id}`);
            modules.push({ chunk: chunk.fileName, module: path.relative(root, id), package: recordPackage(directory) });
          }
        }
      },
    }],
  });
  assert(packages.has("js-yaml@4.3.2"), "Patched YAML library absent from bundle evidence");
  assert(packages.has("lodash@4.18.1"), "Patched lodash absent from bundle evidence");
  const staticPackage = recordPackage(path.join(root, "node_modules/monaco-editor"));
  assert(fs.existsSync("build/assets/vs"), "Dynamically loaded Monaco assets missing");
  // Match the upstream post-build cleanup without invoking npx at build time.
  fs.rmSync("build/frontend/index.baseUrl.html", { force: true });
  const files = {};
  const inventory = directory => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name);
      if (entry.isDirectory()) inventory(file);
      else {
        assert(entry.isFile(), `Unexpected frontend output: ${file}`);
        files[path.relative("build", file)] = createHash("sha256").update(fs.readFileSync(file)).digest("hex");
      }
    }
  };
  inventory("build");
  write("frontend-evidence.json", { sourceCommit: "066c5be9deb14b12bcef3485b531c64a84a2a219",
    version: "0.41.0", modules, staticPackages: [{ package: staticPackage, path: "assets/vs" }], files });
  const components = [...packages.values()].sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`))
    .map(({ name, version }) => ({
      type: "library", name, version,
      "bom-ref": `${name}@${version}`,
      purl: `pkg:npm/${name.replace(/^@/, "%40")}@${version}`,
    }));
  write("frontend.cdx.json", { bomFormat: "CycloneDX", specVersion: "1.6", version: 1, components });
  console.log(JSON.stringify({ renderedPackages: components.length, renderedModules: modules.length, outputFiles: Object.keys(files).length }));
} else {
  throw new Error("Expected prepare or build");
}
