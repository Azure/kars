// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFile } from "node:fs/promises";

const lockPath = process.argv[2] ?? "package-lock.json";
const lock = JSON.parse(await readFile(lockPath, "utf8"));
const isRecord = (value) =>
  value !== null && typeof value === "object" && !Array.isArray(value);
const isNonemptyString = (value) =>
  typeof value === "string" && value.trim().length > 0;
if (!isRecord(lock) || ![2, 3].includes(lock.lockfileVersion) || !isRecord(lock.packages)) {
  throw new Error("npm audit requires a version 2 or 3 lockfile with a packages map");
}
const versions = new Map();

for (const [path, metadata] of Object.entries(lock.packages)) {
  if (!isRecord(metadata)) throw new Error(`Invalid lockfile entry: ${path}`);
  if (!path || metadata.link === true) continue;
  const marker = "node_modules/";
  const index = path.lastIndexOf(marker);
  if (index < 0) continue;
  if (!isNonemptyString(metadata.version)) {
    throw new Error(`Missing package version in lockfile: ${path}`);
  }
  const parts = path.slice(index + marker.length).split("/");
  // npm aliases retain the registry package name in metadata.name.
  const name = metadata.name ??
    (parts[0]?.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0]);
  if (typeof name !== "string" || !/^(?:@[^/@\s]+\/)?[^/@\s]+$/.test(name)) {
    throw new Error(`Invalid package name in lockfile: ${path}`);
  }
  if (!versions.has(name)) versions.set(name, new Set());
  versions.get(name).add(metadata.version);
}
if (versions.size === 0) {
  throw new Error("Lockfile contains no auditable package versions");
}

const payload = Object.fromEntries(
  [...versions.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([name, packageVersions]) => [name, [...packageVersions].sort()]),
);

const endpoint = "https://registry.npmjs.org/-/npm/v1/security/advisories/bulk";
let response;
for (let attempt = 1; attempt <= 4; attempt += 1) {
  try {
    response = await fetch(endpoint, {
      method: "POST",
      headers: {
        accept: "application/json",
        "content-type": "application/json",
      },
      body: JSON.stringify(payload),
      signal: AbortSignal.timeout(60_000),
    });
  } catch (error) {
    if (attempt === 4) {
      throw new Error("npm bulk advisory request failed after four attempts", { cause: error });
    }
    console.error(`npm audit transport failure (${attempt}/4): ${error.message}`);
    await new Promise((resolve) => setTimeout(resolve, attempt * 5_000));
    continue;
  }
  if (response.ok || (response.status !== 429 && response.status < 500)) break;
  if (attempt < 4) {
    console.error(`npm audit HTTP ${response.status} (${attempt}/4); retrying`);
    await response.body?.cancel();
    await new Promise((resolve) => setTimeout(resolve, attempt * 5_000));
  }
}

if (!response.ok) {
  const body = await response.text();
  throw new Error(
    `npm bulk advisory request failed (${response.status}): ${body.slice(0, 500)}`,
  );
}

const result = await response.json();
if (!isRecord(result)) throw new Error("Invalid npm advisory response: expected a package map");
const severities = new Set(["info", "low", "moderate", "high", "critical"]);
const advisories = [];
for (const [name, entries] of Object.entries(result)) {
  if (!versions.has(name) || !Array.isArray(entries)) {
    throw new Error(`Invalid npm advisory response for package: ${name}`);
  }
  for (const advisory of entries) {
    if (!isRecord(advisory) ||
        (Object.hasOwn(advisory, "name") && advisory.name !== name) ||
        !Number.isSafeInteger(advisory.id) || advisory.id <= 0 ||
        !severities.has(advisory.severity) ||
        !isNonemptyString(advisory.title) ||
        !isNonemptyString(advisory.url) ||
        !isNonemptyString(advisory.vulnerable_versions)) {
      throw new Error(`Invalid npm advisory entry for package: ${name}`);
    }
    // npm's bulk fixtures identify the package by the map key, without a
    // redundant name field (npm/metavuln-calculator normalizes it likewise).
    advisories.push({ ...advisory, name });
  }
}
const blocking = advisories.filter((advisory) =>
  advisory.severity === "high" || advisory.severity === "critical",
);

for (const advisory of advisories) {
  console.log(
    `${advisory.severity}: ${advisory.name} - ${advisory.title} (${advisory.url})`,
  );
}
console.log(
  `npm bulk audit: ${Object.keys(payload).length} packages, ${advisories.length} advisories, ${blocking.length} blocking`,
);

if (blocking.length > 0) process.exitCode = 1;
