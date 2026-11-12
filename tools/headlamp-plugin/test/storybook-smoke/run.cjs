// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use strict";

const { spawn } = require("node:child_process");
const { createServer } = require("node:net");
const { setTimeout: delay } = require("node:timers/promises");

const fixtureMarker = "Webpack dev server smoke test";
const storyTitle = "Security/webpack-dev-middleware";
const timeoutMs = 120_000;

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
    { stdio: "inherit" },
  );
  let spawnError;
  server.once("error", (error) => {
    spawnError = error;
  });

  let lastFailure = "Storybook did not publish the fixture story";
  let loggedAssets = false;
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
      const assetReferences = [
        ...iframeHtml.matchAll(/<script\b[^>]*\bsrc=["']([^"']+)["']/gi),
        ...iframeHtml.matchAll(/<link\b(?=[^>]*\brel=["'][^"']*(?:modulepreload|preload)[^"']*["'])[^>]*\bhref=["']([^"']+)["']/gi),
        ...iframeHtml.matchAll(/\bimport\s+(?:[^"'()]+?\s+from\s+)?["']([^"']+\.m?js(?:\?[^"']*)?)["']/gi),
        ...iframeHtml.matchAll(/\bimport\(\s*["']([^"']+\.m?js(?:\?[^"']*)?)["']\s*\)/gi),
      ].map((match) => new URL(match[1], baseUrl));
      const assetPaths = [...new Set(assetReferences)]
        .filter((url) => url.origin === new URL(baseUrl).origin);

      const logAssetDetails = !loggedAssets;
      if (logAssetDetails) {
        const paths = assetPaths.map((url) => url.pathname).join(", ") || "(none)";
        console.log(`Storybook fixture ${story.id}; iframe assets: ${paths}`);
        console.log(`Storybook iframe HTML: ${iframeHtml.slice(0, 3_000).replace(/\s+/g, " ")}`);
        loggedAssets = true;
      }

      for (const assetUrl of assetPaths) {
        const assetResponse = await tryFetch(assetUrl);
        if (!assetResponse?.ok || !assetResponse.headers.get("content-type")?.includes("javascript")) {
          if (logAssetDetails) {
            const status = assetResponse?.status ?? "unavailable";
            const type = assetResponse?.headers.get("content-type") ?? "unavailable";
            console.log(`Skipped ${assetUrl.pathname}: HTTP ${status}, content-type ${type}`);
          }
          continue;
        }

        const asset = await assetResponse.text();
        if (logAssetDetails) {
          console.log(`Fetched ${assetUrl.pathname}: ${asset.length} JavaScript characters; fixture marker ${asset.includes(fixtureMarker) ? "found" : "not found"}`);
        }
        if (asset.includes(fixtureMarker)) {
          console.log(`Storybook served the compiled fixture asset at ${assetUrl.pathname}`);
          return;
        }
      }

      lastFailure = "No Webpack-served JavaScript asset contained the fixture marker";
      await delay(1_000);
    }

    throw new Error(`Timed out after ${timeoutMs}ms: ${lastFailure}`);
  } finally {
    await stopServer(server);
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
