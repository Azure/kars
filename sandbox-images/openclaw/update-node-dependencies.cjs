// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Replace published dependency contents without re-resolving OpenClaw's graph
// or running lifecycle scripts. npm's bundled dependencies ignore root overrides.
const fs = require("node:fs");
const path = require("node:path");
const crypto = require("node:crypto");
const { createRequire } = require("node:module");

function readPackage(directory) {
  return JSON.parse(fs.readFileSync(path.join(directory, "package.json"), "utf8"));
}

function digest(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function checkedPath(root, relative) {
  if (typeof relative !== "string" || !relative || path.isAbsolute(relative) ||
      relative.split("/").some(part => !part || part === "." || part === "..")) {
    throw new Error(`Invalid dependency path: ${relative}`);
  }
  const result = path.join(root, relative);
  if (fs.realpathSync(result) !== result || !fs.statSync(result).isDirectory()) {
    throw new Error(`Dependency must be a real directory inside the npm tree: ${result}`);
  }
  return result;
}

function dependencyDirectory(target, name) {
  for (let current = target; ; current = path.dirname(current)) {
    if (path.basename(current) !== "node_modules") {
      const file = path.join(current, "node_modules", name, "package.json");
      if (fs.existsSync(file)) return path.dirname(file);
    }
    if (path.dirname(current) === current) return null;
  }
}

function packageTree(owner) {
  const directories = [owner];
  function visit(modules) {
    if (!fs.existsSync(modules)) return;
    for (const entry of fs.readdirSync(modules, { withFileTypes: true })) {
      if (!entry.isDirectory() || entry.name.startsWith(".")) continue;
      const directory = path.join(modules, entry.name);
      if (entry.name.startsWith("@")) {
        visit(directory);
      } else {
        readPackage(directory);
        directories.push(directory);
        visit(path.join(directory, "node_modules"));
      }
    }
  }
  visit(path.join(owner, "node_modules"));
  return directories;
}

function verifyDependencies(target, replacement, semver) {
  if (replacement.engines?.node && !semver.satisfies(process.version, replacement.engines.node)) {
    throw new Error(`${replacement.name} ${replacement.version} does not support ${process.version}`);
  }
  for (const [kind, dependencies] of Object.entries({
    dependencies: replacement.dependencies,
    peerDependencies: replacement.peerDependencies,
    optionalDependencies: replacement.optionalDependencies,
  })) {
    for (const [name, range] of Object.entries(dependencies || {})) {
      const directory = dependencyDirectory(target, name);
      const installed = directory && readPackage(directory);
      const optional = kind === "optionalDependencies" ||
        (kind === "dependencies" && Object.hasOwn(replacement.optionalDependencies || {}, name)) ||
        (kind === "peerDependencies" && replacement.peerDependenciesMeta?.[name]?.optional === true);
      if (!installed && optional) continue;
      if (!installed || installed.name !== name || !semver.satisfies(installed.version, range)) {
        throw new Error(`Incompatible ${kind} for ${target}: ${name}@${range} (installed ${installed?.version || "absent"})`);
      }
    }
  }
}

async function main() {
  if (process.argv.length !== 4) {
    throw new Error("Usage: update-node-dependencies.cjs <global-node_modules> <manifest.json>");
  }
  const root = fs.realpathSync(process.argv[2]);
  if (path.basename(root) !== "node_modules") throw new Error("Expected the global node_modules directory");
  const manifestFile = fs.realpathSync(process.argv[3]);
  const source = path.dirname(manifestFile);
  const manifest = JSON.parse(fs.readFileSync(manifestFile, "utf8"));
  if (!Array.isArray(manifest.packages) || !manifest.packages.length) throw new Error("No dependency updates");
  const npmRequire = createRequire(path.join(root, "npm", "package.json"));
  const pacote = npmRequire("pacote");
  const semver = npmRequire("semver");
  const staging = fs.mkdtempSync(path.join(root, ".kars-node-updates-"));
  const plans = [];
  const parents = new Map();
  const changedParents = new Set();
  const targets = new Set();
  const trees = new Map();
  try {
    for (const update of manifest.packages) {
      if (!/^[a-z0-9][a-z0-9._-]*\.tgz$/.test(update.archive) ||
          !/^[a-f0-9]{64}$/.test(update.sha256) ||
          !semver.valid(update.version) || !Array.isArray(update.targets) || !update.targets.length) {
        throw new Error("Invalid dependency update manifest");
      }
      const archive = path.join(source, update.archive);
      if (!fs.lstatSync(archive).isFile() || digest(archive) !== update.sha256) {
        throw new Error(`Dependency archive checksum mismatch: ${update.archive}`);
      }
      const extracted = path.join(staging, String(plans.length));
      await pacote.extract(archive, extracted, {
        cache: path.join(staging, "cache"),
        integrity: `sha256-${Buffer.from(update.sha256, "hex").toString("base64")}`,
        offline: true,
        ignoreScripts: true,
      });
      const replacement = readPackage(extracted);
      if (replacement.name !== update.name || replacement.version !== update.version ||
          fs.existsSync(path.join(extracted, "node_modules"))) {
        throw new Error(`Unexpected replacement package contents: ${update.archive}`);
      }
      for (const destination of update.targets) {
        if (!semver.valid(destination.from)) throw new Error("Invalid previous dependency version");
        const target = checkedPath(root, destination.path);
        if (targets.has(target)) throw new Error(`Duplicate dependency target: ${target}`);
        targets.add(target);
        const installed = readPackage(target);
        if (installed.name !== update.name ||
            ![destination.from, update.version].includes(installed.version)) {
          throw new Error(`Unexpected installed dependency at ${target}: ${installed.name}@${installed.version}`);
        }
        verifyDependencies(target, replacement, semver);
        if (Boolean(destination.parent) === Boolean(destination.owner)) {
          throw new Error("Exactly one dependency parent or bundled-tree owner is required");
        }
        const parent = checkedPath(root, destination.parent || destination.owner);
        if (!target.startsWith(path.join(parent, "node_modules") + path.sep)) {
          throw new Error(`Dependency parent does not own ${target}`);
        }
        if (destination.owner && !trees.has(parent)) trees.set(parent, packageTree(parent));
        const consumers = destination.owner ? trees.get(parent) : [parent];
        let declarations = 0;
        for (const consumer of consumers) {
          const parentManifest = parents.get(consumer) || readPackage(consumer);
          for (const kind of ["dependencies", "optionalDependencies", "peerDependencies"]) {
            const range = parentManifest[kind]?.[update.name];
            if (typeof range !== "string") continue;
            const resolved = dependencyDirectory(consumer, update.name);
            if (!resolved || fs.realpathSync(resolved) !== target) continue;
            declarations++;
            if (!semver.satisfies(update.version, range)) {
              if (range !== destination.from) {
                throw new Error(`Incompatible parent dependency range: ${consumer}: ${range}`);
              }
              parentManifest[kind][update.name] = update.version;
              changedParents.add(consumer);
            }
          }
          parents.set(consumer, parentManifest);
        }
        if (!declarations) throw new Error(`Missing parent dependency declaration: ${target}`);
        plans.push({ target, extracted, name: update.name, from: installed.version, version: update.version });
      }
    }
    // All archives, targets, engines and dependency ranges are checked before
    // replacing anything. Docker discards the layer if any later step fails.
    for (const plan of plans) {
      const next = fs.mkdtempSync(path.join(path.dirname(plan.target), ".kars-node-next-"));
      const backup = next + "-previous";
      try {
        fs.cpSync(plan.extracted, next, { recursive: true });
        const nested = path.join(plan.target, "node_modules");
        if (fs.existsSync(nested)) fs.cpSync(nested, path.join(next, "node_modules"), { recursive: true });
        fs.chmodSync(next, fs.statSync(plan.target).mode & 0o777);
        fs.renameSync(plan.target, backup);
        try {
          fs.renameSync(next, plan.target);
        } catch (error) {
          fs.renameSync(backup, plan.target);
          throw error;
        }
        fs.rmSync(backup, { recursive: true });
      } finally {
        if (fs.existsSync(next)) fs.rmSync(next, { recursive: true });
      }
    }
    for (const [parent, data] of parents) {
      if (changedParents.has(parent)) {
        fs.writeFileSync(path.join(parent, "package.json"), JSON.stringify(data, null, 2) + "\n");
      }
    }
    for (const plan of plans) {
      verifyDependencies(plan.target, readPackage(plan.target), semver);
      console.log(JSON.stringify({ target: plan.target, package: plan.name, before: plan.from, version: plan.version }));
    }
  } finally {
    fs.rmSync(staging, { recursive: true });
  }
}

main().catch(error => {
  console.error(error.stack || error);
  process.exitCode = 1;
});
