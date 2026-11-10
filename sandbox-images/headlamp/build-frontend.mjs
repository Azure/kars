// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";

const root = process.cwd();
const read = file => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, JSON.stringify(value, null, 2) + "\n");
const lintDependencies = [
  "@headlamp-k8s/eslint-config", "@typescript-eslint/eslint-plugin", "eslint", "eslint-config-prettier",
  "eslint-plugin-import", "eslint-plugin-jsx-a11y", "eslint-plugin-react", "eslint-plugin-react-hooks",
  "eslint-plugin-simple-import-sort", "eslint-plugin-unused-imports",
];

export function productionPolyfills(inject, resolve) {
  const buffer = "\0kars-headlamp-buffer";
  const processShim = "\0kars-headlamp-process";
  const global = "\0kars-headlamp-global";
  const modules = new Map([
    [buffer, `import { Buffer } from ${JSON.stringify(resolve("buffer/"))};
export * from ${JSON.stringify(resolve("buffer/"))};
export default Buffer;`],
    [processShim, `import process from ${JSON.stringify(resolve("process/browser.js"))};
export { process, process as default };`],
    [global, "const global = globalThis; export { global, global as default };"],
  ]);
  const aliases = { buffer, process: processShim, stream: resolve("stream-browserify") };
  return {
    name: "kars-headlamp-production-polyfills",
    apply: "build",
    enforce: "pre",
    config() {
      return {
        resolve: {
          alias: Object.entries(aliases).flatMap(([name, replacement]) => [
            { find: name, replacement },
            { find: `node:${name}`, replacement },
          ]),
        },
        build: {
          rollupOptions: {
            plugins: [inject({ Buffer: [buffer, "default"], process: [processShim, "default"], global: [global, "default"] })],
          },
        },
      };
    },
    resolveId(id) {
      assert(!["crypto", "node:crypto"].includes(id), "Headlamp requested crypto: supply a real reviewed implementation, not a browser stub");
      return modules.has(id) ? id : null;
    },
    load(id) {
      return modules.get(id) ?? null;
    },
  };
}

export function verifyProductionLock(manifest, lock) {
  assert.equal(lock.lockfileVersion, 3, "Expected an npm v3 build lock");
  assert.deepEqual(lock.packages[""].dependencies, manifest.dependencies, "Build lock does not match the prepared manifest");
  assert(!lintDependencies.some(name => name in manifest.dependencies), "Lint-only roots remain in the build manifest");
  assert(!Object.keys(manifest.devDependencies ?? {}).length, "Development roots remain in the build manifest");
  assert(!Object.keys(lock.packages[""].devDependencies ?? {}).length, "Development roots remain in the build lock");
  for (const [name, metadata] of Object.entries(lock.packages)) {
    assert(!/(?:^|\/)node_modules\/(?:deepmerge|elliptic)$/.test(name)
      && !["deepmerge", "elliptic"].includes(metadata.name), `Unresolved dependency finding in build lock: ${name}`);
  }
}

const isMain = process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href;
const command = isMain ? process.argv[2] : undefined;

if (command === "prepare") {
  const app = read("../app/package.json");
  assert.equal(app.version, "0.41.0");
  assert.equal(app.productName, "Headlamp");
  const manifest = read("package.json");
  const config = fs.readFileSync("vite.config.ts", "utf8");
  const polyfillImport = "import { nodePolyfills } from 'vite-plugin-node-polyfills';\n";
  const polyfillCall = "    nodePolyfills({\n      include: ['process', 'buffer', 'stream'],\n    }),\n";
  const marker = "// Node polyfills are supplied by kars-build.mjs for the production build.\n";
  let preparedConfig = config;
  if (config.includes(marker)) {
    assert(!config.includes("nodePolyfills") && !config.includes("vite-plugin-node-polyfills"), "Unexpected already-prepared Vite configuration");
  } else {
    assert.equal(config.split(polyfillImport).length, 2, "Upstream polyfill import changed");
    assert.equal(config.split(polyfillCall).length, 2, "Upstream polyfill configuration changed");
    preparedConfig = config.replace(polyfillImport, marker).replace(polyfillCall, "");
  }
  delete manifest.dependencies["vite-plugin-node-polyfills"];
  for (const name of lintDependencies) delete manifest.dependencies[name];
  for (const name of ["eslintConfig", "prettier", "husky", "lint-staged"]) delete manifest[name];
  for (const name of ["axe-core", "domain-browser"]) delete manifest.overrides[name];
  delete manifest.devDependencies;
  manifest.scripts = { build: "node --max-old-space-size=3072 kars-build.mjs build" };
  Object.assign(manifest.dependencies, {
    "@rollup/plugin-inject": "5.0.5",
    buffer: "6.0.3",
    process: "0.11.10",
    "stream-browserify": "3.0.0",
    "js-yaml": "4.3.2",
    lodash: "4.18.1",
    vite: "6.4.5",
    yaml: "2.8.3",
  });
  Object.assign(manifest.overrides, {
    "@babel/core": "7.29.6",
    browserslist: "4.28.7",
    "picomatch@^2.0.0": "2.3.2",
    "picomatch@^4.0.0": "4.0.4",
    postcss: "8.5.23",
    "source-map-js": "1.2.2",
    "yaml@^1.0.0": "1.10.3",
    "yaml@^2.0.0": "$yaml",
    "js-yaml": "$js-yaml",
    lodash: "$lodash",
    "fast-uri@^3.0.0": "3.1.9",
    "nanoid@^3.0.0": "3.3.18",
  });
  write("package.json", manifest);
  fs.writeFileSync("vite.config.ts", preparedConfig);
  fs.rmSync("rsbuild.config.ts", { force: true });
  fs.rmSync(".storybook", { recursive: true, force: true });
  const environment = {
    REACT_APP_HEADLAMP_VERSION: app.version,
    REACT_APP_HEADLAMP_GIT_VERSION: "066c5be9deb14b12bcef3485b531c64a84a2a219",
    REACT_APP_HEADLAMP_PRODUCT_NAME: app.productName,
    REACT_APP_ENABLE_REACT_QUERY_DEVTOOLS: "false",
    REACT_APP_HEADLAMP_SIDEBAR_DEFAULT_OPEN: "true",
  };
  fs.writeFileSync(".env", Object.entries(environment).map(([key, value]) => `${key}=${value}\n`).join(""));
} else if (command === "verify-lock") {
  verifyProductionLock(read("package.json"), read("package-lock.json"));
} else if (command === "build") {
  verifyProductionLock(read("package.json"), read("package-lock.json"));
  const { build } = await import("vite");
  const { default: inject } = await import("@rollup/plugin-inject");
  const require = createRequire(path.join(root, "package.json"));
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
    plugins: [productionPolyfills(inject, require.resolve), {
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
  assert(packages.has("yaml@2.8.3"), "Patched yaml library absent from bundle evidence");
  assert(packages.has("js-yaml@4.3.2"), "Patched js-yaml library absent from bundle evidence");
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
  const buildInputs = Object.fromEntries(["package.json", "package-lock.json", "vite.config.ts"]
    .map(file => [file, createHash("sha256").update(fs.readFileSync(file)).digest("hex")]));
  write("frontend-evidence.json", { sourceCommit: "066c5be9deb14b12bcef3485b531c64a84a2a219",
    version: "0.41.0", buildInputs, modules, staticPackages: [{ package: staticPackage, path: "assets/vs" }], files });
  const components = [...packages.values()].sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`))
    .map(({ name, version }) => ({
      type: "library", name, version,
      "bom-ref": `${name}@${version}`,
      purl: `pkg:npm/${name.replace(/^@/, "%40")}@${version}`,
    }));
  write("frontend.cdx.json", { bomFormat: "CycloneDX", specVersion: "1.6", version: 1, components });
  console.log(JSON.stringify({ renderedPackages: components.length, renderedModules: modules.length, outputFiles: Object.keys(files).length }));
} else if (isMain) {
  throw new Error("Expected prepare, verify-lock or build");
}
