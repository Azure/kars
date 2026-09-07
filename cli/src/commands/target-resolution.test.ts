// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";

const mocked = vi.hoisted(() => ({
  execute: vi.fn(),
  ensureAgtRepo: vi.fn(),
  context: {
    subscription: "sub-b", resourceGroup: "shared-rg", aksCluster: "shared-aks",
    acrName: "mirror", acrLoginServer: "mirror.azurecr.io",
  },
}));

vi.mock("execa", () => ({ execa: mocked.execute }));
vi.mock("../config.js", async importOriginal => ({
  ...await importOriginal<typeof import("../config.js")>(),
  loadContext: () => mocked.context,
}));
vi.mock("../lib/agt-bootstrap.js", () => ({
  ensureAgtRepo: mocked.ensureAgtRepo, ensureAgtWheels: vi.fn(),
}));

import { pushCommand } from "./push.js";
import { upgradeCommand } from "./upgrade.js";

afterEach(() => { vi.restoreAllMocks(); vi.clearAllMocks(); });

describe("real command handlers stop before mutations on target failure", () => {
  for (const command of ["push", "upgrade"] as const) {
    it(`${command} rejects Azure identity A instead of mutating adopted B or the ambient cluster`, async () => {
      mocked.execute.mockResolvedValue({ stdout: JSON.stringify({
        id: "/subscriptions/sub-a/resourceGroups/shared-rg/providers/Microsoft.ContainerService/managedClusters/shared-aks",
        fqdn: "cluster-a.example.test",
      }) });
      vi.spyOn(process, "exit").mockImplementation(code => { throw new Error(`exit:${code}`); });
      const invocation = command === "push"
        ? pushCommand().parseAsync(["--only", "relay", "--apply"], { from: "user" })
        : upgradeCommand().parseAsync(["--yes", "--to", "v1.2.3"], { from: "user" });
      await expect(invocation).rejects.toThrow(command === "push" ? "does not match" : "exit:1");
      expect(mocked.execute).toHaveBeenCalledTimes(1);
      const [bin, args] = mocked.execute.mock.calls[0] as [string, string[]];
      expect(bin).toBe("az");
      expect(args.slice(0, 2)).toEqual(["aks", "show"]);
      expect(args[args.indexOf("--subscription") + 1]).toBe("sub-b");
      expect(mocked.ensureAgtRepo).not.toHaveBeenCalled();
    });

    it(`${command} rejects Kubernetes context A before ACR, Helm, Docker or rollout mutations`, async () => {
      mocked.execute.mockImplementation(async (bin: string, args: string[]) => {
        if (bin === "az" && args[1] === "show") return { stdout: JSON.stringify({
          id: "/subscriptions/sub-b/resourceGroups/shared-rg/providers/Microsoft.ContainerService/managedClusters/shared-aks",
          fqdn: "cluster-b.example.test",
        }) };
        if (bin === "kubectl") return { stdout: "https://cluster-a.example.test" };
        return { stdout: "" };
      });
      vi.spyOn(process, "exit").mockImplementation(code => { throw new Error(`exit:${code}`); });
      const invocation = command === "push"
        ? pushCommand().parseAsync(["--only", "relay", "--apply"], { from: "user" })
        : upgradeCommand().parseAsync(["--yes", "--to", "v1.2.3"], { from: "user" });
      await expect(invocation).rejects.toThrow(command === "push" ? "does not point" : "exit:1");
      for (const [bin, args] of mocked.execute.mock.calls as Array<[string, string[]]>) {
        expect(["helm", "docker"].includes(bin)).toBe(false);
        expect(args.some(arg => ["acr", "rollout", "apply", "set"].includes(arg))).toBe(false);
      }
      expect(mocked.ensureAgtRepo).not.toHaveBeenCalled();
    });
  }
});
