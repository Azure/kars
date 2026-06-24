// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, it, expect } from "vitest";
import {
  usableSkuSet,
  pickUsableVmSize,
  SYSTEM_POOL_VM_PREFERENCES,
  USER_POOL_VM_PREFERENCES,
  type VmSku,
} from "./vm-size.js";

describe("usableSkuSet", () => {
  it("includes SKUs with no restrictions", () => {
    const skus: VmSku[] = [
      { name: "Standard_D4s_v6" },
      { name: "Standard_D2s_v6", restrictions: [] },
    ];
    const usable = usableSkuSet(skus);
    expect(usable.has("standard_d4s_v6")).toBe(true);
    expect(usable.has("standard_d2s_v6")).toBe(true);
  });

  it("excludes SKUs with a Location restriction", () => {
    const skus: VmSku[] = [
      {
        name: "Standard_D4s_v5",
        restrictions: [
          {
            type: "Location",
            reasonCode: "NotAvailableForSubscription",
            restrictionInfo: { locations: ["eastus2"] },
          },
        ],
      },
    ];
    const usable = usableSkuSet(skus);
    expect(usable.has("standard_d4s_v5")).toBe(false);
  });

  it("keeps SKUs that only have a Zone restriction", () => {
    const skus: VmSku[] = [
      { name: "Standard_D4s_v6", restrictions: [{ type: "Zone" }] },
    ];
    expect(usableSkuSet(skus).has("standard_d4s_v6")).toBe(true);
  });

  it("is case-insensitive on names", () => {
    const usable = usableSkuSet([{ name: "STANDARD_D4S_V6" }]);
    expect(usable.has("standard_d4s_v6")).toBe(true);
  });
});

describe("pickUsableVmSize", () => {
  // Models the real restricted subscription from the field report: _v5 blocked,
  // _v6/_v4 of the same families allowed.
  const usable = usableSkuSet([
    { name: "Standard_D2s_v6" },
    { name: "Standard_D2as_v4" },
    { name: "Standard_D4s_v6" },
    { name: "Standard_D4as_v4" },
    {
      name: "Standard_D2as_v5",
      restrictions: [{ type: "Location", reasonCode: "NotAvailableForSubscription" }],
    },
    {
      name: "Standard_D4s_v5",
      restrictions: [{ type: "Location", reasonCode: "NotAvailableForSubscription" }],
    },
  ]);

  it("picks the first available preference for the system pool", () => {
    const sku = pickUsableVmSize({
      usable,
      preferences: SYSTEM_POOL_VM_PREFERENCES,
      poolLabel: "system",
      flagName: "--system-vm-size",
    });
    // D2as_v5 (blocked) and D2s_v5 (absent) are skipped → first available is D2s_v6.
    expect(sku).toBe("Standard_D2s_v6");
  });

  it("picks the first available preference for the user pool", () => {
    const sku = pickUsableVmSize({
      usable,
      preferences: USER_POOL_VM_PREFERENCES,
      poolLabel: "sandbox",
      flagName: "--node-vm-size",
    });
    expect(sku).toBe("Standard_D4s_v6");
  });

  it("honours an available explicit request", () => {
    const sku = pickUsableVmSize({
      usable,
      preferences: USER_POOL_VM_PREFERENCES,
      poolLabel: "sandbox",
      flagName: "--node-vm-size",
      requested: "Standard_D4as_v4",
    });
    expect(sku).toBe("Standard_D4as_v4");
  });

  it("rejects a restricted explicit request and lists alternatives", () => {
    expect(() =>
      pickUsableVmSize({
        usable,
        preferences: USER_POOL_VM_PREFERENCES,
        poolLabel: "sandbox",
        flagName: "--node-vm-size",
        requested: "Standard_D4s_v5",
      }),
    ).toThrowError(/not available .* Available alternatives: .*Standard_D4s_v6/s);
  });

  it("throws when no preference is available", () => {
    expect(() =>
      pickUsableVmSize({
        usable: new Set<string>(),
        preferences: SYSTEM_POOL_VM_PREFERENCES,
        poolLabel: "system",
        flagName: "--system-vm-size",
      }),
    ).toThrowError(/None of the preferred system VM sizes/);
  });

  it("keeps the historical default first when it is available", () => {
    const allUsable = usableSkuSet([
      { name: "Standard_D2as_v5" },
      { name: "Standard_D2s_v6" },
    ]);
    const sku = pickUsableVmSize({
      usable: allUsable,
      preferences: SYSTEM_POOL_VM_PREFERENCES,
      poolLabel: "system",
      flagName: "--system-vm-size",
    });
    expect(sku).toBe("Standard_D2as_v5");
  });
});
