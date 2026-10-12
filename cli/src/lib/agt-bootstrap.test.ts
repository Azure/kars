// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { createHash } from "node:crypto";
import { execa } from "execa";
import { ensureAgtRepo, ensureAgtWheels } from "./agt-bootstrap.js";

describe("AGT exact source pin", () => {
  let root: string;
  let source: string;
  let target: string;
  let pin: string;

  async function git(args: string[], cwd = source): Promise<string> {
    const result = await execa("git", args, { cwd, env: {
      GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: "/dev/null",
      GIT_AUTHOR_NAME: "Kars test", GIT_AUTHOR_EMAIL: "test@example.invalid",
      GIT_COMMITTER_NAME: "Kars test", GIT_COMMITTER_EMAIL: "test@example.invalid",
    } });
    return result.stdout.trim();
  }

  beforeEach(async () => {
    root = fs.mkdtempSync(path.join(os.tmpdir(), "kars-agt-pin-"));
    source = path.join(root, "upstream");
    target = path.join(root, "consumer");
    fs.mkdirSync(source);
    await git(["init", "--quiet"]);
    fs.writeFileSync(path.join(source, "source.txt"), "pinned\n");
    const python = path.join(source, "agent-governance-python", "agent-governance-toolkit-core");
    fs.mkdirSync(python, { recursive: true });
    fs.writeFileSync(path.join(python, "pyproject.toml"), '[project]\nname="agent-governance-toolkit-core"\n');
    const native = path.join(source, "policy-engine", "sdk", "python");
    fs.mkdirSync(native, { recursive: true });
    fs.writeFileSync(path.join(native, "pyproject.toml"), '[project]\nname="agent-control-specification"\n');
    await git(["add", "."]);
    await git(["commit", "--quiet", "-m", "pinned input"]);
    pin = await git(["rev-parse", "HEAD"]);
    fs.mkdirSync(path.join(root, "vendor", "agt"), { recursive: true });
    fs.writeFileSync(path.join(root, "vendor", "agt", "pin.json"), JSON.stringify({
      url: source, branch: "main", sha: pin, shortSha: pin.slice(0, 8),
    }));
  });

  afterEach(() => fs.rmSync(root, { recursive: true, force: true }));

  it("fetches the exact commit even after the upstream branch advances", async () => {
    fs.writeFileSync(path.join(source, "source.txt"), "later revision\n");
    await git(["commit", "--quiet", "-am", "later input"]);
    expect(await ensureAgtRepo(target, root)).toBe(target);
    expect(await git(["rev-parse", "HEAD"], target)).toBe(pin);
    expect(fs.readFileSync(path.join(target, "source.txt"), "utf8")).toBe("pinned\n");
    expect(await git(["status", "--porcelain"], target)).toBe("");
    expect(fs.readdirSync(root).filter(name => name.startsWith(".kars-agt-"))).toEqual([]);
  });

  it("reuses an exact clean checkout without changing it", async () => {
    expect(await ensureAgtRepo(source, root)).toBe(source);
    expect(await git(["rev-parse", "HEAD"])).toBe(pin);
  });

  it("rejects a different revision without resetting or switching it", async () => {
    fs.writeFileSync(path.join(source, "source.txt"), "user revision\n");
    await git(["commit", "--quiet", "-am", "user input"]);
    const head = await git(["rev-parse", "HEAD"]);
    await expect(ensureAgtRepo(source, root)).rejects.toThrow("existing checkout was not changed");
    expect(await git(["rev-parse", "HEAD"])).toBe(head);
    expect(fs.readFileSync(path.join(source, "source.txt"), "utf8")).toBe("user revision\n");
  });

  it.each(["source.txt", "untracked.txt"])("preserves and rejects local changes to %s", async name => {
    fs.writeFileSync(path.join(source, name), "user content\n");
    await expect(ensureAgtRepo(source, root)).rejects.toThrow("has local changes");
    expect(fs.readFileSync(path.join(source, name), "utf8")).toBe("user content\n");
    expect(await git(["rev-parse", "HEAD"])).toBe(pin);
  });

  it("rejects a subdirectory masquerading as an AGT checkout", async () => {
    const nested = path.join(source, "nested");
    fs.mkdirSync(nested);
    await expect(ensureAgtRepo(nested, root)).rejects.toThrow("not the root");
  });

  it("requires a valid pin even for an existing checkout", async () => {
    fs.writeFileSync(path.join(root, "vendor", "agt", "pin.json"), '{"sha":"main","url":"upstream"}');
    await expect(ensureAgtRepo(source, root)).rejects.toThrow("Invalid AGT source pin");
    fs.unlinkSync(path.join(root, "vendor", "agt", "pin.json"));
    await expect(ensureAgtRepo(source, root)).rejects.toThrow("pin.json not found");
  });

  it("never replaces an existing dangling destination symlink", async () => {
    const missing = path.join(root, "missing");
    fs.symlinkSync(missing, target);
    await expect(ensureAgtRepo(target, root)).rejects.toThrow();
    expect(fs.readlinkSync(target)).toBe(missing);
    expect(fs.existsSync(missing)).toBe(false);
    expect(fs.readdirSync(root).filter(name => name.startsWith(".kars-agt-"))).toEqual([]);
  });

  it("removes only its owned staging directory after fetch failure", async () => {
    fs.writeFileSync(path.join(root, "vendor", "agt", "pin.json"), JSON.stringify({
      url: source, sha: "0".repeat(40),
    }));
    await expect(ensureAgtRepo(target, root)).rejects.toThrow();
    expect(fs.existsSync(target)).toBe(false);
    expect(fs.readdirSync(root).filter(name => name.startsWith(".kars-agt-"))).toEqual([]);
    expect(await git(["rev-parse", "HEAD"])).toBe(pin);
  });

  const wheelNames = [
    "agent_governance_toolkit_core", "agent_governance_toolkit_cli",
    "a2a_agentmesh",
  ];

  function nativeFiles(): [string, string][] {
    return ([
      ["amd64", "x86_64", 62], ["arm64", "aarch64", 183],
    ] as const).flatMap(([architecture, platform, machine]) => {
      const wheel = `agent_control_specification-0.4.0b0-cp311-abi3-linux_${platform}.whl`;
      return [
        [`${architecture}/${wheel}`, ""],
        [`${architecture}/build.json`, JSON.stringify({
          wheel, elf_machine: machine, target: `${platform}-unknown-linux-gnu`,
          rust: "1.95.0", maturin: "1.8.7", sha256: createHash("sha256").update("").digest("hex"),
        })],
      ];
    });
  }

  function prepareWheels(cached: string[], built = wheelNames): string {
    const directory = path.join(root, "runtimes", "wheels");
    fs.mkdirSync(directory, { recursive: true });
    for (const name of cached) {
      fs.writeFileSync(path.join(directory, `${name}-5.0.0-py3-none-any.whl`), "");
    }
    for (const [file, content] of nativeFiles()) {
      fs.mkdirSync(path.dirname(path.join(directory, file)), { recursive: true });
      fs.writeFileSync(path.join(directory, file), content);
    }
    fs.writeFileSync(path.join(root, "runtimes", "build-agt-wheels.sh"), [
      "set -eu",
      "printf 'built\\n' > runtimes/built",
      "find runtimes/wheels -name '*.whl' -delete",
      ...built.map(name => `touch runtimes/wheels/${name}-5.0.0-py3-none-any.whl`),
      "mkdir -p runtimes/wheels/amd64 runtimes/wheels/arm64",
      ...nativeFiles().map(([file, content]) => `printf '%s' '${content}' > runtimes/wheels/${file}`),
    ].join("\n"));
    const recipe = createHash("sha256");
    for (const input of [
      "build-agt-wheels.sh", "Dockerfile.agt-native",
      "build-native-agt-wheel.py", "install-agt-wheels.py",
    ]) {
      const file = path.join(root, "runtimes", input);
      if (input !== "build-agt-wheels.sh") fs.writeFileSync(file, "fixture\n");
      recipe.update(input).update("\0").update(fs.readFileSync(file)).update("\0");
    }
    fs.writeFileSync(path.join(directory, ".agt-sha"), `${pin}\n${recipe.digest("hex")}\n`);
    return directory;
  }

  it("reuses only a complete wheel set from the exact checkout and pin", async () => {
    prepareWheels(wheelNames);
    await ensureAgtWheels(source, root);
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(false);
  });

  it.each(wheelNames)("rebuilds a matching-stamp cache missing %s", async missing => {
    const directory = prepareWheels(wheelNames.filter(name => name !== missing));
    await ensureAgtWheels(source, root);
    expect(fs.readdirSync(directory).filter(name => name.endsWith(".whl"))).toHaveLength(3);
    expect(fs.readFileSync(path.join(directory, ".agt-sha"), "utf8"))
      .toMatch(new RegExp(`^${pin}\\n[a-f0-9]{64}\\n$`));
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(true);
  });

  it("rebuilds caches with duplicate versions or unexpected wheels", async () => {
    const directory = prepareWheels(wheelNames);
    fs.writeFileSync(path.join(directory, "a2a_agentmesh-3.0.0-py3-none-any.whl"), "");
    await ensureAgtWheels(source, root);
    expect(fs.readdirSync(directory).filter(name => name.endsWith(".whl"))).toHaveLength(3);
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(true);
  });

  it("does not stamp an incomplete successful build as a valid cache", async () => {
    const directory = prepareWheels([], ["a2a_agentmesh"]);
    await expect(ensureAgtWheels(source, root)).rejects.toThrow("exactly one wheel");
    expect(fs.existsSync(path.join(directory, ".agt-sha"))).toBe(false);
  });

  it.each(["directory", "symlink"])("does not reuse a %s masquerading as a wheel", async kind => {
    const directory = prepareWheels(wheelNames);
    const wheel = path.join(directory, "a2a_agentmesh-5.0.0-py3-none-any.whl");
    fs.unlinkSync(wheel);
    if (kind === "directory") fs.mkdirSync(wheel);
    else fs.symlinkSync(path.join(source, "source.txt"), wheel);
    await ensureAgtWheels(source, root);
    expect(fs.lstatSync(wheel).isFile()).toBe(true);
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(true);
    expect(fs.readFileSync(path.join(source, "source.txt"), "utf8")).toBe("pinned\n");
  });

  it("rejects a complete cache when the source checkout drifted", async () => {
    prepareWheels(wheelNames);
    fs.writeFileSync(path.join(source, "source.txt"), "unreviewed changes\n");
    await expect(ensureAgtWheels(source, root)).rejects.toThrow("has local changes");
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(false);
  });

  it.each(["amd64", "arm64"])("rebuilds when the %s native wheel is missing or modified", async architecture => {
    const directory = prepareWheels(wheelNames);
    const wheel = nativeFiles().find(([file]) => file.startsWith(architecture) && file.endsWith(".whl"))![0];
    fs.unlinkSync(path.join(directory, wheel));
    await ensureAgtWheels(source, root);
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(true);
    fs.unlinkSync(path.join(root, "runtimes", "built"));
    fs.writeFileSync(path.join(directory, wheel), "modified");
    await ensureAgtWheels(source, root);
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(true);
  });

  it("rejects a native receipt for the wrong target or toolchain", async () => {
    const directory = prepareWheels(wheelNames);
    fs.writeFileSync(path.join(directory, "amd64", "build.json"), '{"target":"aarch64-unknown-linux-gnu"}');
    await ensureAgtWheels(source, root);
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(true);
  });

  it.each([
    "build-agt-wheels.sh", "Dockerfile.agt-native",
    "build-native-agt-wheel.py", "install-agt-wheels.py",
  ])("rebuilds a complete cache after changing %s", async input => {
    prepareWheels(wheelNames);
    fs.appendFileSync(path.join(root, "runtimes", input), "\n# changed recipe\n");
    await ensureAgtWheels(source, root);
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(true);
  });

  it("rebuilds the old pin-only stamp without trusting stale native outputs", async () => {
    const directory = prepareWheels(wheelNames);
    fs.writeFileSync(path.join(directory, ".agt-sha"), pin + "\n");
    await ensureAgtWheels(source, root);
    expect(fs.existsSync(path.join(root, "runtimes", "built"))).toBe(true);
  });
});
