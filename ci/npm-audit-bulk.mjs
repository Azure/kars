// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFile } from "node:fs/promises";

const lockPath = process.argv[2] ?? "package-lock.json";
const lock = JSON.parse(await readFile(lockPath, "utf8"));
const versions = new Map();

for (const [path, metadata] of Object.entries(lock.packages ?? {})) {
  if (!path || metadata.link || typeof metadata.version !== "string") continue;
  const marker = "node_modules/";
  const index = path.lastIndexOf(marker);
  if (index < 0) continue;
  const parts = path.slice(index + marker.length).split("/");
  const name = parts[0]?.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
  if (!name) continue;
  if (!versions.has(name)) versions.set(name, new Set());
  versions.get(name).add(metadata.version);
}

const payload = Object.fromEntries(
  [...versions.entries()]
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([name, packageVersions]) => [name, [...packageVersions].sort()]),
);

const endpoint = "https://registry.npmjs.org/-/npm/v1/security/advisories/bulk";
let response;
let lastError;
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
    lastError = error;
    if (attempt < 4) {
      await new Promise((resolve) => setTimeout(resolve, attempt * 5_000));
      continue;
    }
  }
  if (!response) continue;
  if (response.ok || (response.status !== 429 && response.status < 500)) break;
  if (attempt < 4) await new Promise((resolve) => setTimeout(resolve, attempt * 5_000));
}

if (!response) {
  throw new Error("npm bulk advisory request failed after four transport attempts", {
    cause: lastError,
  });
}
if (!response?.ok) {
  const body = await response?.text();
  throw new Error(
    `npm bulk advisory request failed (${response?.status ?? "no response"}): ${body?.slice(0, 500) ?? ""}`,
  );
}

const result = await response.json();
const advisories = Object.values(result).flatMap((entries) =>
  Array.isArray(entries) ? entries : [],
);
const blocking = advisories.filter((advisory) =>
  advisory.severity === "high" || advisory.severity === "critical",
);

for (const advisory of advisories) {
  console.log(
    `${advisory.severity ?? "unknown"}: ${advisory.name ?? "package"} — ${advisory.title ?? advisory.url ?? "advisory"}`,
  );
}
console.log(
  `npm bulk audit: ${Object.keys(payload).length} packages, ${advisories.length} advisories, ${blocking.length} blocking`,
);

if (blocking.length > 0) process.exitCode = 1;
