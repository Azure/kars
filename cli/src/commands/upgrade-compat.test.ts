// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { buildAdoptedAksContext } from "./config.js";
import { buildHelmUpgradeArgs, buildUpgradeContext, preflightFieldManagerConflicts } from "./upgrade.js";
import type { MeshInstallation } from "../lib/mesh-release.js";

const adopted = () => buildAdoptedAksContext({
  subscription: "sub-b", region: "region", resourceGroup: "shared-rg",
  cluster: "shared-aks", acrLoginServer: "mirror.azurecr.io",
});

describe("real adopted-context to upgrade compatibility", () => {
  it("retains subscription B and does not clear unprovided identity or Key Vault", () => {
    const context = buildUpgradeContext(adopted());
    expect(context.subscription).toBe("sub-b");
    const args = buildHelmUpgradeArgs(context, "chart", "v1.2.3");
    expect(args).toContain("--reuse-values");
    expect(args.some(arg => arg.startsWith("azure.workloadIdentity.clientId="))).toBe(false);
    expect(args.some(arg => arg.startsWith("azure.keyVaultCsi.keyVaultName="))).toBe(false);
  });

  it.each([undefined, null, "", " "])("preserves release settings when saved optional values are %s", value => {
    const context = buildUpgradeContext({ ...adopted(), wiClientId: value as string | undefined, keyVaultName: value as string | undefined });
    const args = buildHelmUpgradeArgs(context, "chart");
    expect(args.some(arg => arg.startsWith("azure.workloadIdentity.clientId="))).toBe(false);
    expect(args.some(arg => arg.startsWith("azure.keyVaultCsi.keyVaultName="))).toBe(false);
  });

  it("applies intentional nonempty adopted identity values", () => {
    const context = buildAdoptedAksContext({
      ...adopted(), subscription: "sub-b", region: "region", resourceGroup: "shared-rg",
      cluster: "shared-aks", wiClientId: "new-client", keyVaultName: "selected-vault",
    });
    const args = buildHelmUpgradeArgs(buildUpgradeContext(context), "chart");
    expect(args).toContain("azure.workloadIdentity.clientId=new-client");
    expect(args).toContain("azure.keyVaultCsi.keyVaultName=selected-vault");
  });

  it("updates Helm-owned mesh to the actual imported repositories and selected tag", () => {
    const mesh: MeshInstallation = {
      kind: "helm", namespace: "agentmesh", deployments: [], release: "kars",
      releaseNamespace: "kars-system", values: { agentMesh: { enabled: true } },
    };
    const args = buildHelmUpgradeArgs(buildUpgradeContext(adopted()), "chart", "v1.2.3", { mesh });
    for (const component of ["registry", "relay"]) {
      expect(args).toContain(`agentMesh.${component}.image.repository=mirror.azurecr.io/agentmesh-${component}-agt`);
      expect(args).toContain(`agentMesh.${component}.image.tag=v1.2.3`);
    }
    expect(args).not.toContain("--take-ownership");
    expect(args).not.toContain("--force-recreate");
  });

  it("does not enable or adopt external mesh through an upgrade image override", () => {
    const args = buildHelmUpgradeArgs(buildUpgradeContext(adopted()), "chart", "v1.2.3", {
      mesh: { kind: "legacy", namespace: "agentmesh", deployments: [] },
    });
    expect(args.some(arg => arg.startsWith("agentMesh."))).toBe(false);
  });

  it("preflights the same owned mesh artifact values as the real upgrade", async () => {
    let argv: readonly string[] = [];
    const execute = (async (_bin: string, args: readonly string[]) => {
      argv = args;
      return { stdout: "" };
    }) as unknown as typeof import("execa").execa;
    const mesh: MeshInstallation = {
      kind: "helm", namespace: "agentmesh", deployments: [], release: "kars",
      releaseNamespace: "kars-system", values: { agentMesh: { enabled: true } },
    };
    await preflightFieldManagerConflicts(execute, buildUpgradeContext(adopted()), "chart", "v1.2.3", { mesh });
    expect(argv).toContain("--dry-run=server");
    expect(argv).toContain("agentMesh.relay.image.repository=mirror.azurecr.io/agentmesh-relay-agt");
    expect(argv).toContain("agentMesh.relay.image.tag=v1.2.3");
  });
});
