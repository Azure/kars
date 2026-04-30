// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Tests for plan item q7 — centralized router URL helpers.
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { routerBase, routerUrl, routerWsBase, routerWsUrl } from "./index.js";

const ENV_KEY = "AZURECLAW_ROUTER_URL";

describe("router URL helpers (q7)", () => {
  let original: string | undefined;
  beforeEach(() => {
    original = process.env[ENV_KEY];
    delete process.env[ENV_KEY];
  });
  afterEach(() => {
    if (original === undefined) delete process.env[ENV_KEY];
    else process.env[ENV_KEY] = original;
  });

  it("default base is 127.0.0.1:8443 when env unset", () => {
    expect(routerBase()).toBe("http://127.0.0.1:8443");
  });

  it("honors AZURECLAW_ROUTER_URL override (late-binding)", () => {
    process.env[ENV_KEY] = "http://fake.test:19999";
    expect(routerBase()).toBe("http://fake.test:19999");
    expect(routerUrl("/v1/models")).toBe("http://fake.test:19999/v1/models");
  });

  it("routerUrl resolves paths against base", () => {
    expect(routerUrl("/foo")).toBe("http://127.0.0.1:8443/foo");
    expect(routerUrl("/agt/registry/lookup?amid=abc")).toBe(
      "http://127.0.0.1:8443/agt/registry/lookup?amid=abc",
    );
  });

  it("routerWsBase derives ws:// from http://", () => {
    expect(routerWsBase()).toBe("ws://127.0.0.1:8443");
  });

  it("routerWsBase derives wss:// from https://", () => {
    process.env[ENV_KEY] = "https://router.internal:8443";
    expect(routerWsBase()).toBe("wss://router.internal:8443");
    expect(routerWsUrl("/agt/relay")).toBe("wss://router.internal:8443/agt/relay");
  });

  it("routerWsUrl preserves path and query", () => {
    expect(routerWsUrl("/agt/relay?token=x")).toBe("ws://127.0.0.1:8443/agt/relay?token=x");
  });
});
