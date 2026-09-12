// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { isDeepStrictEqual } from "node:util";

assert.equal(path.basename(process.cwd()), "teams-gateway");
const before = JSON.parse(execFileSync("git", [
  "show", "HEAD:bridge/teams-gateway/package-lock.json",
], { encoding: "utf8" }));
const after = JSON.parse(readFileSync("package-lock.json", "utf8"));

function closure(packages) {
  const seen = new Set();
  const queue = ["node_modules/vitest"];
  while (queue.length) {
    const location = queue.pop();
    if (seen.has(location)) continue;
    const entry = packages[location];
    assert.ok(entry, `Missing dependency metadata: ${location}`);
    seen.add(location);
    for (const name of Object.keys({ ...entry.dependencies, ...entry.optionalDependencies })) {
      let directory = location;
      let resolved;
      while (directory !== ".") {
        const candidate = path.posix.join(directory, "node_modules", name);
        if (Object.hasOwn(packages, candidate)) {
          resolved = candidate;
          break;
        }
        directory = path.posix.dirname(directory);
      }
      resolved ??= Object.hasOwn(packages, `node_modules/${name}`) ? `node_modules/${name}` : undefined;
      if (resolved) queue.push(resolved);
      else assert.ok(Object.hasOwn(entry.optionalDependencies ?? {}, name), `Missing dependency: ${name}`);
    }
  }
  return seen;
}

const allowed = new Set([...closure(before.packages), ...closure(after.packages), "", "node_modules/qs"]);
assert.deepEqual(after.packages[""], {
  ...before.packages[""], devDependencies: { ...before.packages[""].devDependencies, vitest: "4.1.11" },
});
assert.equal(after.packages["node_modules/qs"].version, "6.16.0");
assert.equal(after.packages["node_modules/vitest"].version, "4.1.11");
assert.equal(after.packages["node_modules/@vitest/mocker"].version, "4.1.11");
assert.equal(after.packages["node_modules/vite"].version, before.packages["node_modules/vite"].version);
assert.equal(after.packages["node_modules/rollup"].version, before.packages["node_modules/rollup"].version);
const changed = [];
for (const name of new Set([...Object.keys(before.packages), ...Object.keys(after.packages)])) {
  const old = before.packages[name];
  const current = after.packages[name];
  if (isDeepStrictEqual(old, current)) continue;
  assert.ok(allowed.has(name), `Unrelated package changed: ${name}`);
  if (name !== "" && name !== "node_modules/qs") {
    if (old && old.dev !== true) assert.deepEqual(current, old, `Production dependency changed: ${name}`);
    if (current) assert.equal(current.dev, true, `New production dependency: ${name}`);
  }
  if (current && name !== "") {
    assert.equal(typeof current.version, "string");
    assert.match(current.resolved, /^https:\/\/registry\.npmjs\.org\//);
    assert.match(current.integrity, /^sha512-[A-Za-z0-9+/]+={0,2}$/);
  }
  changed.push({ path: name, before: old?.version ?? null, after: current?.version ?? null });
}
for (const key of new Set([...Object.keys(before), ...Object.keys(after)])) {
  if (key !== "packages") assert.deepEqual(after[key], before[key]);
}
console.log(JSON.stringify({ verified: "Vitest dependency closure and prior qs fix only", changed }));
