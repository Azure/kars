// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";

const read = (name) => JSON.parse(readFileSync(new URL(`../${name}`, import.meta.url), "utf8"));
const manifest = read("package.json");
const lock = read("package-lock.json");
const require = createRequire(import.meta.url);

test("sharp and every selected prebuilt binary retain the patched dependency floor", () => {
  assert.equal(manifest.overrides.sharp, "^0.35.5");
  const sharp = lock.packages["node_modules/sharp"];
  const version = /^0\.35\.(\d+)$/.exec(sharp.version);
  assert.ok(version && Number(version[1]) >= 5, "GHSA-wq5f-xc86-pv6w requires sharp 0.35.5 or newer");
  assert.ok(sharp.integrity);
  for (const [name, selected] of Object.entries(sharp.optionalDependencies)) {
    assert.ok(name.startsWith("@img/sharp-"));
    const binary = lock.packages[`node_modules/${name}`];
    assert.equal(binary.version, selected, name);
    assert.ok(binary.integrity, name);
  }
});

test("the installed sharp matches the lock and processes SVG through librsvg", async () => {
  const sharp = require("sharp");
  assert.equal(sharp.versions.sharp, lock.packages["node_modules/sharp"].version);
  const svg = Buffer.from('<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4" fill="red"/></svg>');
  const output = await sharp(svg).resize(2, 2).png().toBuffer();
  const metadata = await sharp(output).metadata();
  assert.equal(metadata.format, "png");
  assert.equal(metadata.width, 2);
  assert.equal(metadata.height, 2);
  assert.ok(sharp.versions.rsvg, "the actual native librsvg version must be available");
});
