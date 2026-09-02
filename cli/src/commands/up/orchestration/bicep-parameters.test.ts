// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import {
  buildBicepParameters,
  buildProjectedBicepParameters,
  resolvePoolNames,
  validateInfrastructureMode,
} from "../orchestration.js";

describe("up orchestration", () => {
  it("forwards resolved Kubernetes version and node count to Bicep", () => {
    expect(
      buildBicepParameters({
        location: "westus3",
        baseName: "safe",
        vmSize: "Standard_D4s_v3",
        systemVmSize: "Standard_D2s_v3",
        kataVmSize: "Standard_D4as_v6",
        kubernetesVersion: "1.34.7",
        systemNodeCount: 3,
        nodeCount: 2,
        kataNodeCount: 2,
        systemPoolName: "systemlegacy",
        sandboxPoolName: "userlegacy",
        kataPoolName: "katalegacy",
      }),
    ).toEqual([
      "location=westus3",
      "baseName=safe",
      "recoverKeyVault=false",
      "vmSize=Standard_D4s_v3",
      "systemVmSize=Standard_D2s_v3",
      "kataVmSize=Standard_D4as_v6",
      "kubernetesVersion=1.34.7",
      "systemNodeCount=3",
      "nodeCount=2",
      "kataNodeCount=2",
      "systemPoolName=systemlegacy",
      "sandboxPoolName=userlegacy",
      "kataPoolName=katalegacy",
    ]);
  });

  it("rejects contradictory infrastructure modes", () => {
    expect(() =>
      validateInfrastructureMode({
        skipInfra: true,
        forceInfra: true,
      }),
    ).toThrow("--skip-infra and --force-infra cannot be used together");

    expect(() =>
      validateInfrastructureMode({
        skipInfra: true,
        forceInfra: false,
      }),
    ).not.toThrow();
    expect(() =>
      validateInfrastructureMode({
        skipInfra: false,
        forceInfra: true,
      }),
    ).not.toThrow();
  });

  it("preserves resolved pool names and defaults only absent names", () => {
    expect(
      resolvePoolNames({
        systemPoolName: "systemlegacy",
        sandboxPoolName: "userlegacy",
        kataPoolName: "katalegacy",
      }),
    ).toEqual({
      systemPoolName: "systemlegacy",
      sandboxPoolName: "userlegacy",
      kataPoolName: "katalegacy",
    });
    expect(resolvePoolNames({})).toEqual({
      systemPoolName: "system",
      sandboxPoolName: "clawpool",
      kataPoolName: "katapool",
    });
  });

  it("builds deployment parameters from preflight-projected SKUs and pool names", () => {
    expect(
      buildProjectedBicepParameters({
        location: "eastus2",
        baseName: "safe",
        nodeVmSize: "Standard_Restricted_User_SKU",
        systemVmSize: "Standard_Restricted_System_SKU",
        kataVmSize: "Standard_Restricted_Kata_SKU",
        kubernetesVersion: "1.35.3",
        systemNodeCount: 4,
        nodeCount: 1,
        kataNodeCount: 2,
        systemPoolName: "legacysys",
        sandboxPoolName: "legacyuser",
        kataPoolName: "legacykata",
      }),
    ).toEqual([
      "location=eastus2",
      "baseName=safe",
      "recoverKeyVault=false",
      "vmSize=Standard_Restricted_User_SKU",
      "systemVmSize=Standard_Restricted_System_SKU",
      "kataVmSize=Standard_Restricted_Kata_SKU",
      "kubernetesVersion=1.35.3",
      "systemNodeCount=4",
      "nodeCount=1",
      "kataNodeCount=2",
      "systemPoolName=legacysys",
      "sandboxPoolName=legacyuser",
      "kataPoolName=legacykata",
    ]);
    expect(() =>
      buildProjectedBicepParameters({
        location: "eastus2",
        baseName: "safe",
        kubernetesVersion: "1.35.3",
        nodeCount: 1,
        kataNodeCount: 1,
      }),
    ).toThrow("Preflight did not resolve sandbox, system, and Kata VM sizes");
    expect(() =>
      buildProjectedBicepParameters({
        location: "eastus2",
        baseName: "safe",
        nodeVmSize: "Standard_D4s_v3",
        systemVmSize: "Standard_D2s_v3",
        kataVmSize: "Standard_D4as_v6",
        kubernetesVersion: "1.35.3",
        systemNodeCount: 2,
        nodeCount: 1,
      }),
    ).toThrow(
      "Preflight did not resolve a non-negative Kata node count",
    );
  });

  it("forwards the Key Vault recovery decision through projected Bicep parameters", () => {
    expect(
      buildProjectedBicepParameters({
        location: "eastus2",
        baseName: "safe",
        recoverKeyVault: true,
        nodeVmSize: "Standard_D4s_v3",
        systemVmSize: "Standard_D2s_v3",
        kataVmSize: "Standard_D4as_v6",
        kubernetesVersion: "1.35.3",
        systemNodeCount: 2,
        nodeCount: 1,
        kataNodeCount: 0,
      }),
    ).toContain("recoverKeyVault=true");
  });

  it("declares disabled-by-default recovery and forwards it to the Key Vault module", () => {
    const source = readFileSync(
      new URL("../../../../../deploy/bicep/main.bicep", import.meta.url),
      "utf8",
    );
    expect(source).toContain("param recoverKeyVault bool = false");
    expect(source).toContain("recover: recoverKeyVault");
  });

});
