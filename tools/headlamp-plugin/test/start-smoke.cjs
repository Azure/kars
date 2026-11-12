// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use strict";

const { spawn } = require("node:child_process");
const { mkdtemp, mkdir, readFile, rm, stat } = require("node:fs/promises");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { setTimeout: delay } = require("node:timers/promises");

const timeoutMs = 120_000;
const fixtureMarker = "kars-overview";

function appendOutput(current, chunk) {
  return `${current}${chunk}`.slice(-20_000);
}

async function stopProcessGroup(child) {
  if (child.exitCode !== null || child.signalCode !== null || !child.pid) {
    return;
  }

  const signalGroup = (signal) => {
    try {
      if (process.platform === "win32") {
        child.kill(signal);
      } else {
        process.kill(-child.pid, signal);
      }
    } catch (error) {
      if (error.code !== "ESRCH") {
        throw error;
      }
    }
  };

  signalGroup("SIGTERM");
  const stopped = await Promise.race([
    new Promise((resolveExit) => child.once("exit", () => resolveExit(true))),
    delay(5_000).then(() => false),
  ]);
  if (!stopped) {
    signalGroup("SIGKILL");
    await Promise.race([
      new Promise((resolveExit) => child.once("exit", resolveExit)),
      delay(5_000),
    ]);
  }
}

async function main() {
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "kars-headlamp-start-"));
  const dataHome = join(temporaryDirectory, "data");
  const configHome = join(temporaryDirectory, "config");
  let child;
  try {
    await Promise.all([mkdir(dataHome), mkdir(configHome)]);
    const startedAt = Date.now();
    const bundlePath = join(process.cwd(), "dist", "main.js");
    child = spawn("npm", ["run", "start"], {
      cwd: process.cwd(),
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
      env: {
        ...process.env,
        HOME: temporaryDirectory,
        XDG_CONFIG_HOME: configHome,
        XDG_DATA_HOME: dataHome,
        npm_config_offline: "true",
      },
    });

    let output = "";
    let spawnError;
    const handleOutput = (chunk) => {
      output = appendOutput(output, chunk);
    };
    child.stdout.on("data", handleOutput);
    child.stderr.on("data", (chunk) => {
      output = appendOutput(output, chunk);
    });
    child.once("error", (error) => {
      spawnError = error;
    });

    const deadline = Date.now() + timeoutMs;
    let buildReady = false;
    while (Date.now() < deadline) {
      if (spawnError) {
        throw spawnError;
      }
      if (child.exitCode !== null || child.signalCode !== null) {
        throw new Error(`npm run start exited before entering build-watch mode (code ${child.exitCode}, signal ${child.signalCode})\n${output}`);
      }
      if (output.includes("watching for file changes") && output.includes("built in")) {
        try {
          const bundleStats = await stat(bundlePath);
          if (bundleStats.mtimeMs >= startedAt) {
            buildReady = true;
            break;
          }
        } catch (error) {
          if (error.code !== "ENOENT") {
            throw error;
          }
        }
      }
      await delay(250);
    }
    if (!buildReady) {
      throw new Error(`Timed out after ${timeoutMs}ms waiting for npm run start to rebuild in watch mode\n${output}`);
    }

    const bundle = await readFile(bundlePath, "utf8");
    if (!bundle.includes(fixtureMarker)) {
      throw new Error(`headlamp-plugin start did not emit the expected Kars bundle at ${bundlePath}`);
    }

    console.log("npm run start rebuilt the Kars plugin bundle and remained active in Vite watch mode.");
    console.log("The headlamp-plugin start command does not start an HTTP dev server.");
  } finally {
    try {
      if (child) {
        await stopProcessGroup(child);
      }
    } finally {
      await rm(temporaryDirectory, { recursive: true, force: true });
    }
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
