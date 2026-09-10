// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const directory = fileURLToPath(new URL(".", import.meta.url));
const read = (name) => JSON.parse(readFileSync(new URL(name, import.meta.url), "utf8"));
const baseline = (name) => JSON.parse(execFileSync("git", [
  "show", `HEAD:sandbox-images/mcp-everything/${name}`,
], { cwd: directory, encoding: "utf8" }));

const fixed = {
  "@hono/node-server": "1.19.15",
  hono: "4.13.5",
  qs: "6.16.0",
};
const preserved = {
  "@modelcontextprotocol/server-everything": "2026.7.4",
  "fast-uri": "3.1.6",
  "ip-address": "10.3.1",
};

export function verifyLock() {
  const manifest = read("package.json");
  const lock = read("package-lock.json");
  const previous = baseline("package-lock.json");
  assert.deepEqual(manifest, baseline("package.json"), "Generation changed the reviewed manifest");
  assert.deepEqual(manifest.dependencies, {
    "@modelcontextprotocol/server-everything": preserved["@modelcontextprotocol/server-everything"],
  });
  assert.deepEqual(manifest.overrides, {
    ...fixed, "fast-uri": preserved["fast-uri"], "ip-address": preserved["ip-address"],
  });
  assert.equal(lock.lockfileVersion, 3);
  assert.deepEqual(lock.packages[""].dependencies, manifest.dependencies);
  assert.deepEqual(Object.keys(lock.packages).sort(), Object.keys(previous.packages).sort(),
    "Dependency graph changed beyond the reviewed replacements");

  const versions = { ...fixed, ...preserved };
  const counts = Object.fromEntries(Object.keys(versions).map((name) => [name, 0]));
  for (const [path, entry] of Object.entries(lock.packages)) {
    const name = path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length);
    if (!Object.hasOwn(fixed, name)) {
      assert.deepEqual(entry, previous.packages[path], `Unrelated lock entry changed: ${path}`);
    }
    if (Object.hasOwn(versions, name)) {
      assert.equal(entry.version, versions[name], `Unexpected locked version: ${path}`);
      counts[name]++;
    }
    if (path) {
      const url = new URL(entry.resolved);
      assert.equal(url.protocol, "https:");
      assert.equal(url.hostname, "registry.npmjs.org");
      assert.equal(url.username + url.password + url.search + url.hash, "");
      if (Object.hasOwn(fixed, name)) {
        assert.equal(url.href,
          `https://registry.npmjs.org/${name}/-/${name.split("/").at(-1)}-${fixed[name]}.tgz`);
      }
      assert.match(entry.integrity, /^sha512-[A-Za-z0-9+/]+={0,2}$/);
      const encoded = entry.integrity.slice("sha512-".length);
      const digest = Buffer.from(encoded, "base64");
      assert.equal(digest.length, 64);
      assert.equal(digest.toString("base64"), encoded);
    }
  }
  for (const name of Object.keys(versions)) {
    assert.ok(counts[name] > 0, `Required package missing: ${name}`);
  }
  return { versions, packageCount: Object.keys(lock.packages).length - 1 };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  console.log(JSON.stringify(verifyLock()));
}
