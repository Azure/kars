// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//
// bundle-deploy-assets.mjs — copy the repo's static K8s deploy assets into
// the CLI's dist/ so they ship inside the npm package. This is what lets
// `kars dev --release` / `kars up --release` run with NO repo checkout
// (the resolver in src/lib/repo-assets.ts looks here when not in a repo).
//
// Run as part of `npm run build` (after tsc). Idempotent.

import { existsSync, mkdirSync, cpSync, rmSync, readFileSync, lstatSync } from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "yaml";

const here = path.dirname(fileURLToPath(import.meta.url));
const cliRoot = path.resolve(here, "..");
const repoRoot = path.resolve(cliRoot, "..");
const distDeploy = path.join(cliRoot, "dist", "deploy");

const coreChart = path.join(repoRoot, "deploy/helm/kars");
if (existsSync(coreChart)) {
  const manifest = parse(readFileSync(path.join(coreChart, "Chart.yaml"), "utf8"));
  const dependencies = manifest?.dependencies ?? [];
  const lockPath = path.join(coreChart, "Chart.lock");
  const locked = existsSync(lockPath) ? parse(readFileSync(lockPath, "utf8"))?.dependencies : [];
  const valid = (items) => Array.isArray(items) && items.every((item) =>
    typeof item?.name === "string" && /^[a-z0-9][a-z0-9.-]*$/.test(item.name)
    && typeof item.version === "string" && /^\d+\.\d+\.\d+(?:[-+][a-zA-Z0-9.-]+)*$/.test(item.version)
    && typeof item.repository === "string" && item.repository.length > 0)
    && new Set(items.map((item) => item.name)).size === items.length;
  if (!valid(dependencies) || !valid(locked) || dependencies.length !== locked.length
    || dependencies.some((item) => !locked.some((entry) =>
      entry.name === item.name && entry.version === item.version && entry.repository === item.repository))) {
    throw new Error("Core chart dependencies must be exactly pinned and match Chart.lock; run helm dependency build deploy/helm/kars before bundling.");
  }
  for (const item of locked) {
    const archive = path.join(coreChart, "charts", `${item.name}-${item.version}.tgz`);
    if (!existsSync(archive) || !lstatSync(archive).isFile() || lstatSync(archive).size === 0) {
      throw new Error(`Required locked Core dependency ${item.name}-${item.version}.tgz is missing or not a regular archive; run helm dependency build deploy/helm/kars before bundling.`);
    }
  }
}

// Repo-relative asset → bundled under dist/deploy preserving the layout the
// resolver expects (it looks up "deploy/<...>").
const assets = [
  "deploy/helm/kars",            // Helm chart + values-local-dev.yaml + templates + CRDs
  "deploy/bicep",                // AKS/Foundry infra templates for `kars up --release`
  "deploy/agentmesh-agt.yaml",   // AgentMesh relay+registry manifest
  "deploy/agentmesh-ingress.yaml",
  "deploy/monitoring",           // Azure Monitor query documentation; runtime assets live in Core
  "tools/headlamp-plugin/dist/main.js",   // prebuilt Headlamp CRD-view plugin
  "tools/headlamp-plugin/package.json",
];

let copied = 0;
const missing = [];
for (const rel of assets) {
  const src = path.join(repoRoot, rel);
  if (!existsSync(src)) {
    missing.push(rel);
    continue;
  }
  // rel always starts with "deploy/"; place under dist/deploy/<rest>.
  const dest = path.join(cliRoot, "dist", rel);
  mkdirSync(path.dirname(dest), { recursive: true });
  if (rel === "deploy/helm/kars") rmSync(dest, { recursive: true, force: true });
  cpSync(src, dest, { recursive: true });
  copied++;
}

mkdirSync(distDeploy, { recursive: true });
for (const obsolete of ["podmonitor-sandbox-router.yaml", "agentmesh-json-exporter.yaml",
  "grafana-dashboard-configmap.yaml", "grafana-dashboard-kars-ops.json", "grafana-dashboard-kars-fleet.json"]) {
  rmSync(path.join(distDeploy, "monitoring", obsolete), { force: true });
}
console.log(`[bundle-deploy-assets] copied ${copied} asset(s) into dist/deploy`);
if (missing.length) {
  // The chart + agentmesh manifest are REQUIRED for the release K8s path —
  // fail the build if either load-bearing asset is missing so we never
  // publish a broken package.
  const required = ["deploy/helm/kars", "deploy/agentmesh-agt.yaml"];
  const missingRequired = missing.filter((m) => required.includes(m));
  if (missingRequired.length) {
    console.error(
      `[bundle-deploy-assets] ERROR: required asset(s) not found at repo root: ${missingRequired.join(", ")}`,
    );
    process.exit(1);
  }
  console.warn(`[bundle-deploy-assets] (optional, skipped): ${missing.join(", ")}`);
}
