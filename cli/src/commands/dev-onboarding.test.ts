// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { devCommand, requiredToolsFor } from "./dev.js";
import { createCli } from "../cli.js";

const { local, providers } = vi.hoisted(() => ({ local: vi.fn(async () => {}), providers: vi.fn(async () => {}) }));
vi.mock("./dev/local-k8s.js", () => ({ runLocalK8s: local, setupLocalKindBetaProviders: providers }));
afterEach(() => { local.mockClear(); providers.mockClear(); });

function renderHelp(): string {
  let output = "";
  devCommand().configureOutput({ writeOut: (text) => { output += text; } }).outputHelp();
  return output;
}

describe("standalone dev onboarding help", () => {
  it("describes all three supported providers without requiring Azure for GitHub", () => {
    const help = renderHelp();
    expect(help).toContain("GitHub Copilot with an active subscription (device-code login)");
    expect(help).toContain("GitHub Models with a PAT with `models:read` scope");
    expect(help).toContain("Azure AI Foundry / Azure OpenAI deployment");
    expect(help).toContain("GitHub Copilot and GitHub Models do not require an Azure subscription");
    expect(help).not.toContain("between the two providers");
    expect(help).toContain("NOT overwrite your saved credentials");
  });

  it("distinguishes standalone Core from the maintained Bridge beta install", () => {
    const help = renderHelp();
    expect(help).toContain("standalone Core and sandbox chat, not the Bridge");
    expect(help).toContain("Teams, Missions, Inbox and catalogue");
    expect(help).toContain("two-release Helm installation and its identity/enrollment inputs");
    expect(help).toContain("bridge/docs/public-beta-helm.md");
    expect(help).toContain("Kind beta asks for missing Foundry/Azure OpenAI endpoint, API key and default model");
    expect(help).toContain("Bridge inherits the Core configuration");
    expect(help).toContain("install only the separate Bridge Helm release");
    expect(help).toContain("Removing Bridge must leave Core and its workloads intact");
  });

  it("routes explicit provider setup flags to the child command rather than the standalone parent", async () => {
    await createCli().exitOverride().parseAsync(["dev", "setup-providers", "--kind-cluster", "retained-beta", "--kubeconfig", "/private/config",
      "--state-directory", "/private/state"], { from: "user" });
    expect(providers).toHaveBeenCalledExactlyOnceWith({ clusterName: "retained-beta", kubeconfig: "/private/config", stateDirectory: "/private/state" });
    expect(local).not.toHaveBeenCalled();
  });

  it("routes beta to the shared Kind bootstrap before credential/provider prompts", async () => {
    await devCommand().parseAsync(["--release", "--target", "local-k8s", "--beta-config", "reviewed.yaml", "--cluster-name", "fresh-beta"], { from: "user" });
    expect(local).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ betaConfig: "reviewed.yaml", clusterName: "fresh-beta", noBuild: true, ephemeral: false }));
    expect(renderHelp()).toContain("--beta-config <file>");
  });

  it("routes a bare beta flag into managed setup with a fresh unique cluster name", async () => {
    await devCommand().parseAsync(["--release", "--target", "local-k8s", "--beta-config"], { from: "user" });
    expect(local).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ betaConfig: true, clusterName: expect.stringMatching(/^kars-beta-[a-f0-9]{8}$/), noBuild: true }));
    expect(renderHelp()).toContain("--beta-config [file]");
  });

  it.each([
    ["--target", "local-k8s"],
    ["--release", "--target", "docker"],
    ["--release", "v1", "--target", "local-k8s"],
    ["--release", "--target", "local-k8s", "--ephemeral"],
    ["--release", "--target", "local-k8s", "--no-mesh"],
  ])("refuses conflicting beta flags %j", async (...args) => {
    await expect(devCommand().exitOverride().parseAsync([...args, "--beta-config", "reviewed.yaml"], { from: "user" })).rejects.toThrow(/Kind beta requires/);
    expect(local).not.toHaveBeenCalled();
  });

  it("preserves Docker defaults and the explicit no-compile Kind release path", () => {
    const command = devCommand();
    expect(command.options.find((option) => option.long === "--target")?.defaultValue).toBe("docker");
    expect(command.options.find((option) => option.long === "--cluster-name")?.defaultValue).toBe("kars-dev");
    expect(requiredToolsFor("local-k8s", { build: false, noMesh: false, release: true })
      .map(({ bin }) => bin)).toEqual(["kind", "kubectl", "helm"]);
    expect(requiredToolsFor("docker", { build: false, noMesh: false, release: true })
      .map(({ bin }) => bin)).toEqual(["docker"]);
  });
});
