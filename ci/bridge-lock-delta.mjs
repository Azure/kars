// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { basename } from "node:path";

const project = basename(process.cwd());
assert.ok(["web", "teams-gateway"].includes(project));
const path = `bridge/${project}/package-lock.json`;
const before = JSON.parse(execFileSync("git", ["show", `HEAD:${path}`], { encoding: "utf8" }));
const after = JSON.parse(readFileSync("package-lock.json", "utf8"));
const targets = project === "web"
  ? { "node_modules/mermaid": "11.16.1", "node_modules/dompurify": "3.4.13" }
  : { "node_modules/qs": "6.16.0" };

console.log(JSON.stringify({
  versions: Object.fromEntries(Object.keys(targets).map(name => [name, after.packages[name]?.version])),
  added: Object.keys(after.packages).filter(name => !Object.hasOwn(before.packages, name)),
  removed: Object.keys(before.packages).filter(name => !Object.hasOwn(after.packages, name)),
}));
assert.deepEqual(Object.keys(after.packages).sort(), Object.keys(before.packages).sort());
for (const [name, entry] of Object.entries(before.packages)) {
  if (Object.hasOwn(targets, name)) {
    assert.equal(after.packages[name].version, targets[name]);
    assert.match(after.packages[name].resolved, /^https:\/\/registry\.npmjs\.org\//);
    assert.match(after.packages[name].integrity, /^sha512-[A-Za-z0-9+/]+={0,2}$/);
    const previous = { ...entry };
    const current = { ...after.packages[name] };
    for (const key of ["version", "resolved", "integrity"]) {
      delete previous[key];
      delete current[key];
    }
    assert.deepEqual(current, previous, `Unexpected package metadata change: ${name}`);
  } else if (name === "" && project === "web") {
    assert.deepEqual(after.packages[name], {
      ...entry, dependencies: { ...entry.dependencies, mermaid: "11.16.1" },
    });
  } else {
    assert.deepEqual(after.packages[name], entry, `Unrelated package changed: ${name}`);
  }
}
const { packages: oldPackages, ...oldEnvelope } = before;
const { packages: newPackages, ...newEnvelope } = after;
assert.deepEqual(newEnvelope, oldEnvelope);
console.log(`Verified bounded ${project} lock delta: ${Object.keys(targets).join(", ")}`);
