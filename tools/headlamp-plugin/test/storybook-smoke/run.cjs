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
  const baseOrigin = new URL(baseUrl).origin;
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
      stdio: ["inherit", "inherit", "inherit", "ipc"],
      env: {
        ...process.env,
        STORYBOOK_DISABLE_TELEMETRY: "1",
      },
    },
  );
  let spawnError;
  let assetManifest;
  let manifestError;
  server.once("error", (error) => {
    spawnError = error;
  });
  server.on("message", (message) => {
    if (message?.type !== "kars-storybook-smoke-assets") {
      return;
    }
    if (typeof message.hasErrors !== "boolean" || !Array.isArray(message.storyAssets)
      || !message.storyAssets.every((asset) => typeof asset === "string")) {
      manifestError = new Error("Storybook sent an invalid emitted-asset manifest");
      return;
    }
    assetManifest = message;
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

      if (manifestError) {
        throw manifestError;
      }
      if (!assetManifest) {
        lastFailure = "Storybook's Webpack compilation has not reported the fixture asset manifest";
        await delay(1_000);
        continue;
      }
      if (assetManifest.hasErrors) {
        throw new Error("Storybook's Webpack compilation reported errors");
      }
      if (assetManifest.storyAssets.length === 0) {
        throw new Error("Webpack did not emit an asset containing the fixture story module");
      }

      console.log(`Storybook fixture ${story.id}; emitted story chunk(s): ${assetManifest.storyAssets.join(", ")}`);
      for (const asset of assetManifest.storyAssets) {
        const assetUrl = new URL(asset, baseUrl);
        if (assetUrl.origin !== baseOrigin || assetUrl.username || assetUrl.password) {
          throw new Error(`Webpack emitted a nonlocal fixture chunk URL: ${assetUrl.href}`);
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
      }

      throw new Error(`Storybook's emitted fixture chunk(s) did not contain the expected marker: ${assetManifest.storyAssets.join(", ")}`);
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
