// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import { explicitContextArg } from "./kube-bootstrap.js";

describe("explicitContextArg", () => {
  it("reads a separate --context value", () => {
    expect(explicitContextArg(["node", "kars", "operator", "--context", "aks-prod"]))
      .toBe("aks-prod");
  });

  it("reads an inline --context value", () => {
    expect(explicitContextArg(["node", "kars", "operator", "--context=aks-prod"]))
      .toBe("aks-prod");
  });

  it("rejects missing or empty values", () => {
    expect(explicitContextArg(["node", "kars", "operator"])).toBeUndefined();
    expect(explicitContextArg(["node", "kars", "operator", "--context"])).toBeUndefined();
    expect(explicitContextArg(["node", "kars", "operator", "--context="])).toBeUndefined();
  });
});
