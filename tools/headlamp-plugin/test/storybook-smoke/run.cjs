// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use strict";

const { mkdtemp, readFile, rmdir, unlink } = require("node:fs/promises");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { spawn } = require("node:child_process");
const { createServer } = require("node:net");
const { setTimeout: delay } = require("node:timers/promises");

const fixtureMarker = "Webpack dev server smoke test";
const fixtureAsset = "webpack-dev-middleware-stories.iframe.bundle.js";
const storyTitle = "Security/webpack-dev-middleware";
const timeoutMs = 120_000;

async function readAssetManifest(manifestPath) {
  try {
    return JSON.parse(await readFile(manifestPath, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") {
      return null;
    }
    throw error;
  }
}

async function getAvailablePort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const { port } = server.address();
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return port;
}

async function tryFetch(url) {
  try {
    return await fetch(url, { signal: AbortSignal.timeout(2_000) });
  } catch (error) {
    if (error instanceof TypeError || error.name === "AbortError" || error.name === "TimeoutError") {
      return null;
    }
    throw error;
  }
}

async function stopServer(server) {
  if (server.exitCode !== null || server.signalCode !== null || !server.pid) {
    return;
  }

  server.kill("SIGTERM");
  const stopped = await Promise.race([
    new Promise((resolve) => server.once("exit", () => resolve(true))),
    delay(5_000).then(() => false),
  ]);
  if (!stopped) {
    server.kill("SIGKILL");
  }
}

async function main() {
  const port = await getAvailablePort();
  const baseUrl = `http://127.0.0.1:${port}/`;
  const baseOrigin = new URL(baseUrl).origin;
  const manifestDirectory = await mkdtemp(join(tmpdir(), "kars-storybook-smoke-"));
  const manifestPath = join(manifestDirectory, "asset-manifest.json");
  const server = spawn(
    "./node_modules/.bin/storybook",
    [
      "dev",
      "--config-dir",
      "test/storybook-smoke",
      "--ci",
      "--no-open",
      "--host",
      "127.0.0.1",
      "--port",
      String(port),
    ],
    {
      stdio: "inherit",
      env: {
        ...process.env,
        KARS_STORYBOOK_SMOKE_ASSET_MANIFEST: manifestPath,
        STORYBOOK_DISABLE_TELEMETRY: "1",
      },
    },
  );
  let spawnError;
  server.once("error", (error) => {
    spawnError = error;
  });

  let lastFailure = "Storybook did not publish the fixture story";
  const deadline = Date.now() + timeoutMs;
  try {
    while (Date.now() < deadline) {
      if (spawnError) {
        throw spawnError;
      }
      if (server.exitCode !== null || server.signalCode !== null) {
        throw new Error(
          `Storybook exited before serving the fixture (code ${server.exitCode}, signal ${server.signalCode})`,
        );
      }

      const indexResponse = await tryFetch(new URL("index.json", baseUrl));
      if (!indexResponse?.ok) {
        lastFailure = "Storybook's story index is not ready";
        await delay(1_000);
        continue;
      }

      const index = await indexResponse.json();
      const story = Object.values(index.entries ?? {}).find(
        (entry) => entry.type === "story" && entry.title === storyTitle && entry.name === "DevServer",
      );
      if (!story) {
        lastFailure = `Storybook index does not contain ${storyTitle}/DevServer`;
        await delay(1_000);
        continue;
      }

      const iframeResponse = await tryFetch(
        new URL(`iframe.html?id=${encodeURIComponent(story.id)}&viewMode=story`, baseUrl),
      );
      if (!iframeResponse?.ok) {
        lastFailure = `Storybook did not serve the iframe for ${story.id}`;
        await delay(1_000);
        continue;
      }

      const iframeHtml = await iframeResponse.text();
      const iframeAssets = [
        ...iframeHtml.matchAll(/<script\b[^>]*\bsrc=["']([^"']+)["']/gi),
        ...iframeHtml.matchAll(/<link\b(?=[^>]*\brel=["'][^"']*(?:modulepreload|preload)[^"']*["'])[^>]*\bhref=["']([^"']+)["']/gi),
        ...iframeHtml.matchAll(/\bimport\s+(?:[^"'()]+?\s+from\s+)?["']([^"']+\.m?js(?:\?[^"']*)?)["']/gi),
        ...iframeHtml.matchAll(/\bimport\(\s*["']([^"']+\.m?js(?:\?[^"']*)?)["']\s*\)/gi),
      ].map((match) => new URL(match[1], baseUrl));
      const assetPaths = [...new Set(iframeAssets)]
        .filter((url) => url.origin === new URL(baseUrl).origin);
      if (assetPaths.length === 0) {
        lastFailure = `Storybook iframe for ${story.id} did not reference a Webpack bundle`;
        await delay(1_000);
        continue;
      }

      const manifest = await readAssetManifest(manifestPath);
      if (!manifest) {
        lastFailure = "Storybook's Webpack compilation has not emitted the fixture asset manifest";
        await delay(1_000);
        continue;
      }
      if (manifest.hasErrors) {
        throw new Error("Storybook's Webpack compilation reported errors");
      }
      if (!manifest.storyAssets.includes(fixtureAsset)) {
        throw new Error(`Webpack did not emit the expected fixture asset: ${manifest.storyAssets.join(", ")}`);
      }

      console.log(`Storybook fixture ${story.id}; emitted story chunk(s): ${manifest.storyAssets.join(", ")}`);
      const assetUrl = new URL(fixtureAsset, baseUrl);
      if (assetUrl.origin !== baseOrigin) {
        throw new Error(`Fixture asset URL is not local: ${assetUrl.href}`);
      }
      const assetResponse = await tryFetch(assetUrl);
      if (!assetResponse?.ok) {
        throw new Error(`Storybook did not serve emitted fixture chunk ${assetUrl.pathname} (HTTP ${assetResponse?.status ?? "unavailable"})`);
      }
      if (!assetResponse.headers.get("content-type")?.includes("javascript")) {
        throw new Error(`Storybook served ${assetUrl.pathname} with a non-JavaScript content type`);
      }
      const body = await assetResponse.text();
      console.log(`Fetched ${assetUrl.pathname}: fixture marker ${body.includes(fixtureMarker) ? "found" : "not found"}`);
      if (body.includes(fixtureMarker)) {
        console.log(`Storybook served the compiled fixture asset at ${assetUrl.pathname}`);
        return;
      }

      throw new Error(`Storybook's emitted fixture chunk did not contain the expected marker: ${assetUrl.pathname}`);
    }

    throw new Error(`Timed out after ${timeoutMs}ms: ${lastFailure}`);
  } finally {
    await stopServer(server);
    for (const path of [manifestPath, `${manifestPath}.tmp`]) {
      await unlink(path).catch((error) => {
        if (error.code !== "ENOENT") {
          throw error;
        }
      });
    }
    await rmdir(manifestDirectory);
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
