// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";

import {
  createAzureRunner,
  createSubscriptionPinnedExeca,
  pinAzureSubscription,
} from "../orchestration.js";

vi.mock("../preflight.js", () => ({
  isValidAzureHost: () => true,
}));
describe("up orchestration", () => {
  it("pins Azure commands exactly once to the preflight-selected subscription", async () => {
    expect(pinAzureSubscription(["aks", "show"], "sub-1")).toEqual([
      "aks",
      "show",
      "--subscription",
      "sub-1",
    ]);
    expect(
      pinAzureSubscription(
        ["group", "show", "--subscription", "sub-1"],
        "sub-1",
      ),
    ).toEqual(["group", "show", "--subscription", "sub-1"]);
    expect(
      pinAzureSubscription(
        ["group", "show", "--subscription=sub-1"],
        "sub-1",
      ),
    ).toEqual(["group", "show", "--subscription=sub-1"]);
    expect(() =>
      pinAzureSubscription(
        ["group", "show", "--subscription", "other-sub"],
        "sub-1",
      ),
    ).toThrow("not the deployment subscription");
    expect(() =>
      pinAzureSubscription(
        [
          "group",
          "show",
          "--subscription",
          "sub-1",
          "--subscription=sub-1",
        ],
        "sub-1",
      ),
    ).toThrow("duplicate --subscription");
  });

  it("automatically pins every command executed by an Azure runner", async () => {
    const execute = vi.fn().mockResolvedValue({ stdout: "ok" });
    const runAzure = createAzureRunner(
      execute as unknown as typeof import("execa").execa,
      "preflight-sub",
    );

    await expect(
      runAzure(["deployment", "group", "create"], { timeout: 1234 }),
    ).resolves.toEqual({ stdout: "ok" });
    expect(execute).toHaveBeenCalledWith(
      "az",
      [
        "deployment",
        "group",
        "create",
        "--subscription",
        "preflight-sub",
      ],
      { stdio: "pipe", timeout: 1234 },
    );
  });

  it("pins Azure calls made by injected helper dependencies", async () => {
    const execute = vi.fn().mockResolvedValue({ stdout: "{}" });
    const scoped = createSubscriptionPinnedExeca(
      execute as unknown as typeof import("execa").execa,
      "preflight-sub",
    );

    await scoped("az", ["rest", "--method", "get", "--url", "/resource"]);
    await scoped("kubectl", ["get", "pods"]);

    expect(execute.mock.calls[0][1]).toEqual([
      "rest",
      "--method",
      "get",
      "--url",
      "/resource",
      "--subscription",
      "preflight-sub",
    ]);
    expect(execute.mock.calls[1][1]).toEqual(["get", "pods"]);
  });

  it("has no direct unscoped Azure CLI calls in downstream up modules", () => {
    const downstream = [
      "../../up.js",
      "../images.js",
      "../sandbox_bringup.js",
      "../agentmesh_deploy.js",
    ];
    for (const modulePath of downstream) {
      const sourcePath = new URL(modulePath.replace(/\.js$/, ".ts"), import.meta.url);
      const source = readFileSync(sourcePath, "utf8");
      expect(source, modulePath).not.toMatch(/\bexeca\(\s*["']az["']/);
    }
  });

  it("exposes the deployment-safety flags with early node-count parsing", async () => {
    const { upCommand } = await import("../../up.js");
    const command = upCommand();
    const help = command.helpInformation();

    expect(help).toContain("--kubernetes-version <version>");
    expect(help).toContain("--node-count <count>");
    expect(help).toContain("--rollback-on-failure");
    expect(help).toMatch(
      /Use a unique resource group generated for this\s+invocation/,
    );
    expect(help).toMatch(
      /Cannot be combined with\s+an explicit or cached resource group/,
    );
    expect(help).toContain("--kata-vm-size <sku>");
    expect(help).not.toContain("--kata-node-count");
    expect(help).not.toContain("--system-pool-name");
    expect(help).not.toContain("--sandbox-pool-name");
    expect(help).not.toContain("--kata-pool-name");

    const option = command.options.find(
      (candidate) => candidate.long === "--node-count",
    );
    expect(option?.parseArg?.("2", undefined)).toBe(2);
    expect(() => option?.parseArg?.("0", undefined)).toThrow(
      "must be an integer from 1 to 100",
    );
    expect(() => option?.parseArg?.("101", undefined)).toThrow(
      "must be an integer from 1 to 100",
    );
  });

  it("rejects contradictory CLI infrastructure flags before Azure work", async () => {
    const { upCommand } = await import("../../up.js");
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    const exit = vi
      .spyOn(process, "exit")
      .mockImplementation((code): never => {
        throw new Error(`process.exit(${String(code)})`);
      });
    try {
      await expect(
        upCommand().parseAsync([
          "node",
          "kars",
          "--skip-infra",
          "--force-infra",
        ]),
      ).rejects.toThrow("process.exit(1)");
      expect(consoleError).toHaveBeenCalledWith(
        expect.stringContaining(
          "--skip-infra and --force-infra cannot be used together",
        ),
      );
    } finally {
      exit.mockRestore();
      consoleError.mockRestore();
    }
  });

});
