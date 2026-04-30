// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it } from "vitest";
import {
  DEFAULT_PROVIDER_SELECTION,
  DEV_ONLY_LABEL_KEY,
  DEV_ONLY_LABEL_VALUE,
  parseOutageMode,
  parseProviderKind,
  selectionHasNull,
  selectionToEnv,
} from "./providers.js";

describe("providers — parseProviderKind", () => {
  it("accepts canonical values", () => {
    expect(parseProviderKind("vendored")).toBe("vendored");
    expect(parseProviderKind("agt")).toBe("agt");
    expect(parseProviderKind("null")).toBe("null");
  });

  it("tolerates whitespace and case", () => {
    expect(parseProviderKind("  Vendored\n")).toBe("vendored");
    expect(parseProviderKind("AGT")).toBe("agt");
  });

  it("rejects aliases the controller accepts (noop / disabled) on purpose", () => {
    expect(parseProviderKind("noop")).toBeNull();
    expect(parseProviderKind("disabled")).toBeNull();
    expect(parseProviderKind("")).toBeNull();
    expect(parseProviderKind("custom")).toBeNull();
  });
});

describe("providers — default selection", () => {
  it("is vendored across the board", () => {
    expect(DEFAULT_PROVIDER_SELECTION.mesh).toBe("vendored");
    expect(DEFAULT_PROVIDER_SELECTION.policy).toBe("vendored");
    expect(DEFAULT_PROVIDER_SELECTION.audit).toBe("vendored");
    expect(DEFAULT_PROVIDER_SELECTION.signing).toBe("vendored");
    expect(selectionHasNull(DEFAULT_PROVIDER_SELECTION)).toBe(false);
  });

  it("is frozen so accidental mutation fails loudly in strict mode", () => {
    expect(Object.isFrozen(DEFAULT_PROVIDER_SELECTION)).toBe(true);
  });
});

describe("providers — selectionHasNull", () => {
  it("flags a selection where any field is null", () => {
    expect(selectionHasNull({ ...DEFAULT_PROVIDER_SELECTION, audit: "null" })).toBe(true);
    expect(selectionHasNull({ ...DEFAULT_PROVIDER_SELECTION, mesh: "null" })).toBe(true);
  });

  it("returns false when all fields are non-null", () => {
    expect(selectionHasNull({ ...DEFAULT_PROVIDER_SELECTION, policy: "agt" })).toBe(false);
  });
});

describe("providers — selectionToEnv", () => {
  it("emits the four AZURECLAW_PROVIDER_* env vars", () => {
    const env = selectionToEnv({
      mesh: "vendored",
      policy: "agt",
      audit: "vendored",
      signing: "agt",
    });
    expect(env).toEqual({
      AZURECLAW_PROVIDER_MESH: "vendored",
      AZURECLAW_PROVIDER_POLICY: "agt",
      AZURECLAW_PROVIDER_AUDIT: "vendored",
      AZURECLAW_PROVIDER_SIGNING: "agt",
    });
  });
});

describe("providers — outage mode", () => {
  it("parses the three supported modes, tolerating hyphen variants", () => {
    expect(parseOutageMode("strict")).toBe("strict");
    expect(parseOutageMode("cached-read")).toBe("cached-read");
    expect(parseOutageMode("cachedread")).toBe("cached-read");
    expect(parseOutageMode("degraded-dev")).toBe("degraded-dev");
    expect(parseOutageMode("degradeddev")).toBe("degraded-dev");
  });

  it("rejects unknown modes", () => {
    expect(parseOutageMode("lenient")).toBeNull();
    expect(parseOutageMode("")).toBeNull();
  });
});

describe("providers — dev-only label constants", () => {
  it("match the controller-side / ci/no-null-provider-prod.sh expectations", () => {
    expect(DEV_ONLY_LABEL_KEY).toBe("azureclaw.azure.com/dev-only");
    expect(DEV_ONLY_LABEL_VALUE).toBe("true");
  });
});
