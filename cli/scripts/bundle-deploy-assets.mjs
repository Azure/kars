// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
//
// bundle-deploy-assets.mjs — copy the repo's static K8s deploy assets into
// the CLI's dist/ so they ship inside the npm package. This is what lets
// `kars dev --release` / `kars up --release` run with NO repo checkout
// (the resolver in src/lib/repo-assets.ts looks here when not in a repo).
//
// Run as part of `npm run build` (after tsc). Idempotent.

import { existsSync, mkdirSync, cpSync } from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const cliRoot = path.resolve(here, "..");
const repoRoot = path.resolve(cliRoot, "..");
const distDeploy = path.join(cliRoot, "dist", "deploy");

// Repo-relative asset → bundled under dist/deploy preserving the layout the
// resolver expects (it looks up "deploy/<...>").
const assets = [
  "deploy/helm/kars",            // Helm chart + values-local-dev.yaml + templates + CRDs
  "deploy/bicep",                // AKS/Foundry infra templates for `kars up --release`
  "deploy/agentmesh-agt.yaml",   // AgentMesh relay+registry manifest
  "deploy/agentmesh-ingress.yaml",
  "deploy/monitoring",           // PodMonitor + Grafana dashboards (best-effort)
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
  cpSync(src, dest, { recursive: true });
  copied++;
}

mkdirSync(distDeploy, { recursive: true });
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
