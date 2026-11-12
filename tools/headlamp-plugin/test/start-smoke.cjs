// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use strict";

const { spawn } = require("node:child_process");
const { mkdtemp, mkdir, readFile, rm, stat } = require("node:fs/promises");
const { tmpdir } = require("node:os");
const { createRequire } = require("node:module");
const { basename, isAbsolute, join, relative, resolve, sep } = require("node:path");
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
    const savedEnvironment = Object.fromEntries(
      ["HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME"].map((key) => [key, process.env[key]]),
    );
    let headlampPaths;
    try {
      Object.assign(process.env, {
        HOME: temporaryDirectory,
        XDG_DATA_HOME: dataHome,
        XDG_CONFIG_HOME: configHome,
      });
      const toolkitRequire = createRequire(require.resolve("@kinvolk/headlamp-plugin/package.json"));
      headlampPaths = toolkitRequire("env-paths")("Headlamp", { suffix: "" });
    } finally {
      for (const [key, value] of Object.entries(savedEnvironment)) {
        if (value === undefined) {
          delete process.env[key];
        } else {
          process.env[key] = value;
        }
      }
    }

    const relativeDataPath = relative(temporaryDirectory, headlampPaths.data);
    if (isAbsolute(relativeDataPath) || relativeDataPath.startsWith(`..${sep}`) || relativeDataPath === "..") {
      throw new Error(`Headlamp resolved its plugin data path outside the isolated directory: ${headlampPaths.data}`);
    }
    const packageJson = JSON.parse(await readFile("package.json", "utf8"));
    const pluginName = packageJson.name.split("/").pop();
    await mkdir(join(headlampPaths.data, "plugins", pluginName), { recursive: true });

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
    let pendingOutput = "";
    let copiedBundlePath;
    let copyPathError;
    let spawnError;
    const handleOutput = (chunk) => {
      output = appendOutput(output, chunk);
      pendingOutput += chunk;
      const lines = pendingOutput.split(/\r?\n/);
      pendingOutput = lines.pop();
      for (const line of lines) {
        const match = line.match(/^Copied ".*" -> "(.+)"\.$/);
        if (match && basename(match[1]) === "main.js") {
          const candidate = resolve(match[1]);
          const candidateRelativePath = relative(temporaryDirectory, candidate);
          if (isAbsolute(candidateRelativePath) || candidateRelativePath.startsWith(`..${sep}`) || candidateRelativePath === "..") {
            copyPathError = new Error(`headlamp-plugin start wrote outside its isolated config directory: ${candidate}`);
          } else {
            copiedBundlePath = candidate;
          }
        }
      }
    };
    child.stdout.on("data", handleOutput);
    child.stderr.on("data", (chunk) => {
      output = appendOutput(output, chunk);
    });
    child.once("error", (error) => {
      spawnError = error;
    });

    const deadline = Date.now() + timeoutMs;
    let copied = false;
    while (Date.now() < deadline) {
      if (spawnError) {
        throw spawnError;
      }
      if (copyPathError) {
        throw copyPathError;
      }
      if (child.exitCode !== null || child.signalCode !== null) {
        throw new Error(`npm run start exited before copying the plugin bundle (code ${child.exitCode}, signal ${child.signalCode})\n${output}`);
      }
      if (copiedBundlePath) {
        try {
          await stat(copiedBundlePath);
          copied = true;
          break;
        } catch (error) {
          if (error.code !== "ENOENT") {
            throw error;
          }
        }
      }
      await delay(250);
    }
    if (!copied) {
      throw new Error(`Timed out after ${timeoutMs}ms waiting for npm run start to copy the plugin bundle\n${output}`);
    }

    const [copiedBundle, sourceBundle] = await Promise.all([
      readFile(copiedBundlePath, "utf8"),
      readFile(join(process.cwd(), "dist", "main.js"), "utf8"),
    ]);
    if (!copiedBundle.includes(fixtureMarker)) {
      throw new Error(`headlamp-plugin start did not emit the expected Kars bundle at ${copiedBundlePath}`);
    }
    if (copiedBundle !== sourceBundle) {
      throw new Error(`headlamp-plugin start's copied bundle differs from dist/main.js at ${copiedBundlePath}`);
    }

    const relativeBundlePath = relative(temporaryDirectory, copiedBundlePath);
    console.log(`npm run start rebuilt and copied the Vite plugin bundle to its isolated Headlamp config directory: ${relativeBundlePath}`);
    console.log("The headlamp-plugin start command uses Vite build-watch mode; it does not start an HTTP dev server.");
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
