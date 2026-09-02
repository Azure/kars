// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import {
  KATA_POOL_VM_SIZE,
  parseRegionalVmFamilyQuotas,
  parseVmSkuCapacities,
  selectAksKubernetesVersion,
  validateAutomaticAksNodeResourceGroupName,
  validateDerivedAzureResourceNames,
  type VmSkuCapacity,
} from "./deployment-safety.js";

const system: VmSkuCapacity = {
  name: "Standard_D2s_v3",
  family: "standardDSv3Family",
  vcpus: 2,
};

describe("validateDerivedAzureResourceNames", () => {
  it("accepts the 14-character baseName Key Vault boundary", () => {
    const names = validateDerivedAzureResourceNames("abcdefghijklmn-aks");
    expect(names.baseName).toBe("abcdefghijklmn");
    expect(names.keyVaultExample).toHaveLength(24);
  });

  it("rejects 15 characters with actionable cluster-name guidance", () => {
    expect(() =>
      validateDerivedAzureResourceNames("abcdefghijklmno"),
    ).toThrowError(
      /Key Vault.*25 characters.*at most 24.*--cluster-name.*at most 14/s,
    );
  });

  it("rejects an empty or syntactically invalid derived baseName", () => {
    expect(() => validateDerivedAzureResourceNames("-aks")).toThrowError(
      /Derived baseName.*invalid/,
    );
    expect(() => validateDerivedAzureResourceNames("Bad_Name")).toThrowError(
      /lowercase letters/,
    );
    expect(() => validateDerivedAzureResourceNames("1cluster")).toThrowError(
      /must start with a lowercase letter/,
    );
    expect(() => validateDerivedAzureResourceNames("bad--name")).toThrowError(
      /single internal hyphens/,
    );
  });
});
describe("validateAutomaticAksNodeResourceGroupName", () => {
  it("accepts the 80-character boundary", () => {
    const resourceGroup = "r".repeat(60);
    const name = validateAutomaticAksNodeResourceGroupName(
      resourceGroup,
      "kars-aks",
      "westus3",
    );

    expect(name).toHaveLength(80);
  });

  it("rejects an overlong custom region with actionable guidance", () => {
    const region = "customregion".repeat(6);
    expect(() =>
      validateAutomaticAksNodeResourceGroupName(
        `kars-${region}`,
        "kars-aks",
        region,
      ),
    ).toThrowError(
      /AKS automatic node resource group.*at most 80.*--resource-group.*--cluster-name.*--region/s,
    );
  });
});

describe("selectAksKubernetesVersion", () => {
  const response = {
    values: [
      {
        version: "1.35",
        capabilities: { supportPlan: ["KubernetesOfficial", "AKSLongTermSupport"] },
        patchVersions: {
          "1.35.4": {},
          "1.35.6": {},
        },
      },
      {
        version: "1.36",
        capabilities: { supportPlan: [{ name: "KubernetesOfficial" }] },
        patchVersions: {
          "1.36.1": {},
          "1.36.2": { isPreview: true },
        },
      },
      {
        version: "1.34",
        capabilities: { supportPlan: ["AKSLongTermSupport"] },
        patchVersions: { "1.34.9": {} },
      },
    ],
  };

  it("selects the newest stable KubernetesOfficial patch", () => {
    expect(selectAksKubernetesVersion(response)).toBe("1.36.1");
  });

  it("selects the highest numeric patch from the live Azure CLI schema", () => {
    const liveWestUs3Shape = {
      values: [
        {
          version: "1.36",
          isPreview: null,
          capabilities: {
            supportPlan: ["KubernetesOfficial", "AKSLongTermSupport"],
          },
          patchVersions: {
            "1.36.0": {},
            "1.36.3": {},
          },
        },
        {
          version: "1.33",
          isPreview: null,
          capabilities: { supportPlan: ["AKSLongTermSupport"] },
          patchVersions: { "1.33.13": {} },
        },
      ],
    };
    expect(selectAksKubernetesVersion(liveWestUs3Shape)).toBe("1.36.3");
    expect(() =>
      selectAksKubernetesVersion(liveWestUs3Shape, "1.33.13"),
    ).toThrowError(/not available.*KubernetesOfficial/);
  });

  it("supports the aks-preview valuesProperty live schema", () => {
    const liveAksPreviewShape = {
      valuesProperty: [
        {
          version: "1.36",
          capabilities: {
            supportPlan: ["KubernetesOfficial", "AKSLongTermSupport"],
          },
          patchVersions: {
            "1.36.1": {},
            "1.36.4": {},
          },
        },
        {
          version: "1.35",
          capabilities: { supportPlan: ["AKSLongTermSupport"] },
          patchVersions: {
            "1.35.9": {},
          },
        },
      ],
    };
    expect(selectAksKubernetesVersion(liveAksPreviewShape)).toBe("1.36.4");
    expect(() =>
      selectAksKubernetesVersion(liveAksPreviewShape, "1.35.9"),
    ).toThrowError(/not available.*KubernetesOfficial/);
  });

  it("accepts an explicit standard-support minor or patch", () => {
    expect(selectAksKubernetesVersion(response, "1.35")).toBe("1.35");
    expect(selectAksKubernetesVersion(response, "1.35.4")).toBe("1.35.4");
    expect(selectAksKubernetesVersion(response, "v1.35.4")).toBe("1.35.4");
  });

  it("rejects LTS-only, preview, and unknown explicit versions", () => {
    expect(() => selectAksKubernetesVersion(response, "1.34")).toThrowError(
      /not available.*KubernetesOfficial/,
    );
    expect(() => selectAksKubernetesVersion(response, "1.36.2")).toThrowError(
      /not available.*KubernetesOfficial/,
    );
    expect(() => selectAksKubernetesVersion(response, "1.37")).toThrowError(
      /Supported versions include/,
    );
  });

  it("fails closed when Azure reports no standard-support versions", () => {
    expect(() =>
      selectAksKubernetesVersion({
        values: [{ version: "1.33", capabilities: { supportPlan: ["AKSLongTermSupport"] } }],
      }),
    ).toThrowError(/no stable KubernetesOfficial/);
  });
});

describe("VM metadata and quota parsing", () => {
  it("extracts family/vCPU metadata and regional remaining quota", () => {
    const capacities = parseVmSkuCapacities(
      [
        {
          name: "Standard_D2s_v3",
          family: "standardDSv3Family",
          capabilities: [{ name: "vCPUs", value: "2" }],
        },
      ],
      ["Standard_D2s_v3"],
    );
    expect(capacities.get("standard_d2s_v3")).toEqual(system);

    const parsedQuotas = parseRegionalVmFamilyQuotas([
      {
        name: { value: "standardDSv3Family" },
        currentValue: "3",
        limit: "10",
      },
      {
        name: { value: "cores" },
        currentValue: "8",
        limit: "20",
      },
    ]);
    expect(parsedQuotas.get("standarddsv3family")?.remaining).toBe(7);
    expect(parsedQuotas.get("cores")?.remaining).toBe(12);
  });

  it("fails closed when confidential Kata pool metadata is absent", () => {
    expect(() =>
      parseVmSkuCapacities(
        [
          {
            name: system.name,
            family: system.family,
            capabilities: [{ name: "vCPUs", value: "2" }],
          },
        ],
        [system.name, KATA_POOL_VM_SIZE],
      ),
    ).toThrowError(
      /metadata did not include family\/vCPU details.*Standard_D4as_v6/,
    );
  });
});
