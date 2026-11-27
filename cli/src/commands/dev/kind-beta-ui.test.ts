// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { afterEach, describe, expect, it, vi } from "vitest";
import { probeKindBetaUI } from "./kind-beta.js";

afterEach(() => vi.restoreAllMocks());

describe("loopback UI readiness probe", () => {
  it.each([200, 302, 399])("accepts HTTP %s without following redirects or reading the body", async status => {
    const cancel = vi.fn(async () => {});
    const fetch = vi.spyOn(globalThis, "fetch").mockResolvedValue({ status, body: { cancel } } as unknown as Response);
    await expect(probeKindBetaUI(new URL("http://localhost:3000/private"))).resolves.toBeUndefined();
    expect(fetch).toHaveBeenCalledWith(new URL("http://127.0.0.1:3000"), expect.objectContaining({ redirect: "manual" }));
    expect(cancel).toHaveBeenCalledOnce();
  });
  it.each([401, 403, 500, 502, 503, 504])("retains a fixed HTTP %s category for auth and retry handling", async status => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue({ status, body: null } as Response);
    await expect(probeKindBetaUI(new URL("http://localhost:3000"))).rejects.toThrow(`Kind beta UI is not responsive (HTTP ${status})`);
  });
  it.each([199, 400, 501, 599, NaN, "untrusted-private-value", "__proto__"])("does not persist unrecognized HTTP status %s", async status => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue({ status, body: null } as unknown as Response);
    await expect(probeKindBetaUI(new URL("http://localhost:3000"))).rejects.toThrow("Kind beta UI is not responsive (unexpected HTTP status)");
  });
});
