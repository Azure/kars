// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import test from "node:test";

const require = createRequire(import.meta.url);
const read = (path) => readFileSync(new URL(path, import.meta.url), "utf8");

for (const [family, symbol, alias] of [
  ["sans", "GeistSans", "geistSans"],
  ["mono", "GeistMono", "geistMono"],
]) {
  test(`${family} typography uses a bundled variable font with the existing CSS contract`, () => {
    const layout = read("../src/app/layout.tsx");
    assert.ok(layout.includes(`import { ${symbol} as ${alias} } from "geist/font/${family}"`));
    assert.ok(layout.includes(`${alias}.variable`));
    assert.ok(!layout.includes("next/font/google"));
    assert.ok(read("../src/app/globals.css").includes(`--font-${family}: var(--font-geist-${family})`));

    const loaderPath = require.resolve(`geist/font/${family}`);
    const loader = readFileSync(loaderPath, "utf8");
    assert.match(loader, /import localFont from "next\/font\/local"/);
    assert.ok(loader.includes(`variable: "--font-geist-${family}"`));
    assert.match(loader, /weight: "100 900"/);
    const assetPath = loader.match(/src: "([^"]+\.woff2)"/);
    assert.ok(assetPath, "the maintained loader must use a local WOFF2 asset");
    const asset = readFileSync(resolve(dirname(loaderPath), assetPath[1]));
    assert.equal(asset.subarray(0, 4).toString("ascii"), "wOF2");
    assert.equal(asset.readUInt32BE(8), asset.length);
  });
}

test("font dependencies are locked and production image compilation has no network", () => {
  const manifest = JSON.parse(read("../package.json"));
  const lock = JSON.parse(read("../package-lock.json"));
  const installedPath = resolve(dirname(require.resolve("geist/font/sans")), "../package.json");
  const installed = JSON.parse(readFileSync(installedPath, "utf8"));
  assert.equal(manifest.dependencies.geist, lock.packages["node_modules/geist"].version);
  assert.equal(installed.version, manifest.dependencies.geist);
  assert.ok(lock.packages["node_modules/geist"].integrity);
  assert.match(read("../Dockerfile"), /^RUN --network=none npm run build$/m);
});
