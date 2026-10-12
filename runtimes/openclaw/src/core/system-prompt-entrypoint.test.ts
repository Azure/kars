// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

const entrypoint = readFileSync(new URL("../../../../sandbox-images/openclaw/entrypoint.sh", import.meta.url), "utf8");
const start = entrypoint.indexOf("  # Bind the authoritative prompt after text substitutions;");
const end = entrypoint.indexOf("  # Set provider credentials via environment", start);
if (start < 0 || end <= start) throw new Error("Missing entrypoint prompt binding block");
const binding = entrypoint.slice(start, end);
const directories: string[] = [];

const original = {
  agents: { defaults: { model: { primary: "azure-openai/gpt-4.1" } } },
  plugins: {
    allow: ["kars", "telegram", "brave"],
    entries: {
      kars: { enabled: true, config: { sandboxName: "named-agent" } },
      telegram: { enabled: true },
      brave: { enabled: true, hooks: { allowConversationAccess: false, allowPromptInjection: false } },
    },
  },
  channels: { telegram: { enabled: true, botToken: "fixture-only" } },
};

function runBinding(prompt: string | null, config = JSON.stringify(original)) {
  const directory = mkdtempSync(join(tmpdir(), "kars-prompt-test-"));
  directories.push(directory);
  const configPath = join(directory, "openclaw.json");
  const promptPath = join(directory, "kars-system-prompt.txt");
  writeFileSync(configPath, config);
  if (prompt !== null) writeFileSync(promptPath, prompt);
  const result = spawnSync("/bin/bash", ["-e", "-u", "-o", "pipefail", "-s"], {
    input: binding,
    encoding: "utf8",
    timeout: 10_000,
    env: { ...process.env, OPENCLAW_CONFIG: configPath, SYSTEM_PROMPT_FILE: promptPath },
  });
  return { result, directory, configPath, content: readFileSync(configPath, "utf8") };
}

afterEach(() => {
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

describe("entrypoint system prompt binding", () => {
  it("preserves exact instructions and existing configuration, granting hooks only to Kars", () => {
    const prompt = 'Kars 🐾\nUse "named agents" & C:\\work\\repo | artifacts.\nKeep `tools` and ${literal} unchanged.\n';
    const { result, content, configPath, directory } = runBinding(prompt);
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toBe("");
    expect(JSON.parse(content)).toEqual({
      ...original,
      plugins: {
        ...original.plugins,
        entries: {
          ...original.plugins.entries,
          kars: {
            ...original.plugins.entries.kars,
            config: { ...original.plugins.entries.kars.config, systemPrompt: prompt },
            hooks: { allowConversationAccess: true, allowPromptInjection: true },
          },
        },
      },
    });
    expect(statSync(configPath).mode & 0o777).toBe(0o600);
    expect(readdirSync(directory).sort()).toEqual(["kars-system-prompt.txt", "openclaw.json"]);
  });

  it.each(["", " \n\t", null])("refuses empty or missing instructions without replacing config (%j)", (prompt) => {
    const { result, directory, content } = runBinding(prompt);
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("FATAL: unable to configure the Kars system prompt");
    expect(content).toBe(JSON.stringify(original));
    expect(readdirSync(directory).filter((name) => name.startsWith("openclaw.json."))).toEqual([]);
  });

  it.each(["", '{"plugins":'])("preserves invalid config and removes the failed temporary file (%j)", (config) => {
    const { result, content, directory } = runBinding("Kars instructions", config);
    expect(result.status).not.toBe(0);
    expect(content).toBe(config);
    expect(readdirSync(directory).sort()).toEqual(["kars-system-prompt.txt", "openclaw.json"]);
  });

  it("retains credential-driven tool plugins without evaluating credential text", () => {
    const pluginStart = entrypoint.indexOf("  for plugin_pair in \\");
    const pluginEnd = entrypoint.indexOf("\n  sed -i", pluginStart);
    expect(pluginStart).toBeGreaterThan(0);
    expect(pluginEnd).toBeGreaterThan(pluginStart);
    const result = spawnSync("/bin/bash", ["-e", "-u", "-o", "pipefail", "-s"], {
      input: `PLUGINS_LIST='"kars"'\nPLUGINS_ENTRIES='"kars": {"enabled": true}'\n${entrypoint.slice(pluginStart, pluginEnd)}\nprintf '{"allow":[%s],"entries":{%s}}\\n' "$PLUGINS_LIST" "$PLUGINS_ENTRIES"`,
      encoding: "utf8",
      timeout: 10_000,
      env: {
        ...process.env,
        BRAVE_API_KEY: 'fixture & "quotes" \\ paths | 🐾',
        TAVILY_API_KEY: "fixture\nsecond line",
        EXA_API_KEY: "", FIRECRAWL_API_KEY: "", PERPLEXITY_API_KEY: "", OPENAI_API_KEY: "",
      },
    });
    expect(result.status, result.stderr).toBe(0);
    expect(JSON.parse(result.stdout)).toEqual({
      allow: ["kars", "brave", "tavily"],
      entries: { kars: { enabled: true }, brave: { enabled: true }, tavily: { enabled: true } },
    });
  });

  it("declares the plugin config field and removes the unsupported host setting", () => {
    const manifest = JSON.parse(readFileSync(new URL("../../openclaw.plugin.json", import.meta.url), "utf8"));
    expect(manifest.configSchema.properties.systemPrompt.type).toBe("string");
    expect(manifest.configSchema.additionalProperties).toBe(false);
    expect(entrypoint).not.toContain("systemPromptOverride");
    expect(entrypoint).not.toContain("SYSTEM_PROMPT_JSON");
  });
});
