// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

"use strict";

const { spawn } = require("node:child_process");
const { existsSync } = require("node:fs");
const { mkdtemp, mkdir, readFile, rm, stat, writeFile } = require("node:fs/promises");
const { createRequire } = require("node:module");
const { tmpdir } = require("node:os");
const { isAbsolute, join, relative, sep } = require("node:path");
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

function getHeadlampPaths(temporaryDirectory, dataHome, configHome) {
  const overrides = {
    HOME: temporaryDirectory,
    USERPROFILE: temporaryDirectory,
    XDG_CONFIG_HOME: configHome,
    XDG_DATA_HOME: dataHome,
    APPDATA: configHome,
    LOCALAPPDATA: dataHome,
  };
  const previousEnvironment = Object.fromEntries(
    Object.keys(overrides).map((key) => [key, process.env[key]]),
  );
  try {
    Object.assign(process.env, overrides);
    const toolkitRequire = createRequire(require.resolve("@kinvolk/headlamp-plugin/package.json"));
    const envPaths = toolkitRequire("env-paths");
    const paths = envPaths("Headlamp", { suffix: "" });
    return {
      ...paths,
      configDir: existsSync(paths.data) ? paths.data : paths.config,
    };
  } finally {
    for (const [key, value] of Object.entries(previousEnvironment)) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  }
}

function isWithin(directory, filePath) {
  const relativePath = relative(directory, filePath);
  return !isAbsolute(relativePath) && relativePath !== ".." && !relativePath.startsWith(`..${sep}`);
}

function completedBuilds(output) {
  return (output.match(/built in/g) || []).length;
}

async function waitForCopiedBundle({
  child,
  getOutput,
  getSpawnError,
  bundlePath,
  copiedBundlePath,
  builds,
  notBefore,
}) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (getSpawnError()) {
      throw getSpawnError();
    }
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(
        `npm run start exited before completing ${builds} build(s) (code ${child.exitCode}, signal ${child.signalCode})\n${getOutput()}`
      );
    }
    if (completedBuilds(getOutput()) >= builds) {
      try {
        const [sourceStats, copiedStats] = await Promise.all([
          stat(bundlePath),
          stat(copiedBundlePath),
        ]);
        if (
          sourceStats.mtimeMs >= notBefore - 1_000 &&
          copiedStats.mtimeMs >= notBefore - 1_000
        ) {
          const [sourceBundle, copiedBundle] = await Promise.all([
            readFile(bundlePath, "utf8"),
            readFile(copiedBundlePath, "utf8"),
          ]);
          if (sourceBundle === copiedBundle && sourceBundle.includes(fixtureMarker)) {
            return;
          }
        }
      } catch (error) {
        if (error.code !== "ENOENT") {
          throw error;
        }
      }
    }
    await delay(250);
  }
  throw new Error(
    `Timed out after ${timeoutMs}ms waiting for build ${builds} and its copied plugin bundle\n${getOutput()}`
  );
}

async function main() {
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "kars-headlamp-start-"));
  const dataHome = join(temporaryDirectory, "data");
  const configHome = join(temporaryDirectory, "config");
  const sourcePath = join(process.cwd(), "src", "index.tsx");
  let originalSource;
  let sourceModified = false;
  let child;
  try {
    await Promise.all([mkdir(dataHome), mkdir(configHome)]);
    const paths = getHeadlampPaths(temporaryDirectory, dataHome, configHome);
    const packageJson = JSON.parse(await readFile("package.json", "utf8"));
    const pluginName = packageJson.name.split("/").pop();
    const bundlePath = join(process.cwd(), "dist", "main.js");
    const copiedBundlePath = join(paths.configDir, "plugins", pluginName, "main.js");
    if (!isWithin(temporaryDirectory, copiedBundlePath)) {
      throw new Error(
        `Headlamp resolved its plugin path outside the isolated directory: ${copiedBundlePath}`
      );
    }
    await mkdir(paths.configDir, { recursive: true });
    originalSource = await readFile(sourcePath);
    const startedAt = Date.now();
    child = spawn("npm", ["run", "start"], {
      cwd: process.cwd(),
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
      env: {
        ...process.env,
        HOME: temporaryDirectory,
        USERPROFILE: temporaryDirectory,
        XDG_CONFIG_HOME: configHome,
        XDG_DATA_HOME: dataHome,
        APPDATA: configHome,
        LOCALAPPDATA: dataHome,
        npm_config_offline: "true",
      },
    });

    let output = "";
    let spawnError;
    child.stdout.on("data", (chunk) => {
      output = appendOutput(output, chunk);
    });
    child.stderr.on("data", (chunk) => {
      output = appendOutput(output, chunk);
    });
    child.once("error", (error) => {
      spawnError = error;
    });

    await waitForCopiedBundle({
      child,
      getOutput: () => output,
      getSpawnError: () => spawnError,
      bundlePath,
      copiedBundlePath,
      builds: 1,
      notBefore: startedAt,
    });
    const rebuildSource = Buffer.concat([
      originalSource,
      Buffer.from(`\n// headlamp-start-smoke-${process.pid}\n`),
    ]);
    sourceModified = true;
    await writeFile(sourcePath, rebuildSource);
    const rebuildStartedAt = Date.now();
    await waitForCopiedBundle({
      child,
      getOutput: () => output,
      getSpawnError: () => spawnError,
      bundlePath,
      copiedBundlePath,
      builds: 2,
      notBefore: rebuildStartedAt,
    });

    await writeFile(sourcePath, originalSource);
    sourceModified = false;
    await waitForCopiedBundle({
      child,
      getOutput: () => output,
      getSpawnError: () => spawnError,
      bundlePath,
      copiedBundlePath,
      builds: 3,
      notBefore: Date.now(),
    });

    console.log(
      "npm run start built and copied the Kars bundle, then rebuilt and recopied it after a source change and restore."
    );
    console.log("The headlamp-plugin start command does not start an HTTP dev server.");
  } finally {
    try {
      if (child) {
        await stopProcessGroup(child);
      }
    } finally {
      try {
        if (sourceModified) {
          await writeFile(sourcePath, originalSource);
        }
      } finally {
        await rm(temporaryDirectory, { recursive: true, force: true });
      }
    }
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
