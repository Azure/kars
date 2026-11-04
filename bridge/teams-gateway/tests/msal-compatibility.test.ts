// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { generateKeyPairSync } from "node:crypto";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { App } from "@microsoft/teams.apps";
import { PUBLIC } from "@microsoft/teams.api";
import { ConsoleLogger } from "@microsoft/teams.common";
import { ServiceTokenValidator } from "@microsoft/teams.apps/dist/middleware/auth/service-token-validator.js";
import type { ConfidentialClientApplication, Configuration } from "@azure/msal-node";
import type { NetworkRequestOptions } from "@azure/msal-common";
import jwt from "jsonwebtoken";

// Resolve the same CommonJS MSAL instance as the Teams SDK, not a parallel ESM copy.
const sdkRequire = createRequire(createRequire(import.meta.url).resolve("@microsoft/teams.apps"));
const msal: typeof import("@azure/msal-node") = sdkRequire("@azure/msal-node");
const clientId = "11111111-1111-4111-8111-111111111111";
const tenantId = "22222222-2222-4222-8222-222222222222";
const authority = `https://login.microsoftonline.com/${tenantId}`;
const secret = "test-only-client-secret";
const logger = new ConsoleLogger("msal-compatibility", { level: "error" });

class SdkApp extends App {
  get tokens() {
    return this.tokenManager;
  }
}

function tokenFixture(audience: string, claims: object = {}): string {
  const encode = (value: object) => Buffer.from(JSON.stringify(value)).toString("base64url");
  return `${encode({ alg: "RS256", typ: "JWT" })}.${encode({
    aud: audience,
    iss: authority,
    exp: Math.floor(Date.now() / 1000) + 3600,
    ...claims,
  })}.test-only-signature`;
}

function networkFixture(fail = false, nonce?: string) {
  const posts: { url: string; body: URLSearchParams }[] = [];
  const network = {
    async sendGetRequestAsync<T>(url: string) {
      expect(new URL(url).hostname).toBe("login.microsoftonline.com");
      const body = url.includes("/discovery/instance")
        ? {
            tenant_discovery_endpoint: `${authority}/v2.0/.well-known/openid-configuration`,
            metadata: [{ preferred_network: "login.microsoftonline.com", preferred_cache: "login.windows.net", aliases: ["login.microsoftonline.com", "login.windows.net"] }],
          }
        : {
            authorization_endpoint: `${authority}/oauth2/v2.0/authorize`,
            token_endpoint: `${authority}/oauth2/v2.0/token`,
            issuer: `${authority}/v2.0`,
            jwks_uri: `${authority}/discovery/v2.0/keys`,
          };
      return { headers: {}, status: 200, body: body as T };
    },
    async sendPostRequestAsync<T>(url: string, options?: NetworkRequestOptions) {
      expect(new URL(url).hostname).toBe("login.microsoftonline.com");
      const body = new URLSearchParams(options?.body);
      posts.push({ url, body });
      const response = fail
        ? { error: "invalid_client", error_description: "Test identity provider rejected credentials" }
        : {
            access_token: tokenFixture(body.get("scope") ?? ""),
            token_type: "Bearer",
            expires_in: 3600,
            scope: body.get("scope"),
            ...(nonce === undefined ? {} : { id_token: tokenFixture(clientId, { nonce }) }),
          };
      return { headers: {}, status: fail ? 401 : 200, body: response as T };
    },
  };
  return { network, posts };
}

function sdkWithNetwork(fail = false) {
  const app = new SdkApp({ clientId, clientSecret: secret, tenantId, logger });
  const fixture = networkFixture(fail);
  // Test-only access to the SDK-created client replaces transport, not token acquisition,
  // credential construction, authority selection, caching, or MSAL response validation.
  const manager = app.tokens as unknown as {
    getConfidentialClient(credentials: object, tenant: string): ConfidentialClientApplication;
  };
  function attach(tenant: string) {
    const client = manager.getConfidentialClient(app.tokens.credentials!, tenant);
    expect(client).toBeInstanceOf(msal.ConfidentialClientApplication);
    const config = (client as unknown as { config: Configuration }).config;
    expect(config.auth.clientSecret).toBe(secret);
    expect(config.auth.authority).toBe(`https://login.microsoftonline.com/${tenant}`);
    expect(config.system?.loggerOptions?.piiLoggingEnabled).toBe(false);
    config.system!.networkClient = fixture.network;
  }
  attach(tenantId);
  return { app, attach, ...fixture };
}

describe("Teams SDK with patched MSAL", () => {
  it("resolves the patched version for the SDK and every locked MSAL copy", () => {
    expect(sdkRequire("@azure/msal-node/package.json").version).toBe("7.1.0");
    const lock = JSON.parse(readFileSync(new URL("../package-lock.json", import.meta.url), "utf8"));
    const copies = Object.entries(lock.packages).filter(([path]) => path.endsWith("node_modules/@azure/msal-node"));
    expect(copies).toHaveLength(1);
    expect(copies[0]?.[1]).toMatchObject({ version: "7.1.0", resolved: "https://registry.npmjs.org/@azure/msal-node/-/msal-node-7.1.0.tgz" });
  });

  it("acquires and caches bot and Graph client-credential tokens with SDK tenant/scope selection", async () => {
    const { app, posts } = sdkWithNetwork();
    const bot = await app.tokens.getBotToken();
    const cachedBot = await app.tokens.getBotToken();
    const graph = await app.tokens.getGraphToken();
    expect(bot).not.toBeNull();
    expect(cachedBot).toEqual(bot);
    expect(graph).not.toBeNull();
    expect(posts).toHaveLength(2);
    expect(posts.map(({ body }) => body.get("scope"))).toEqual([PUBLIC.botScope, PUBLIC.graphScope]);
    for (const { url, body } of posts) {
      expect(url.split("?")[0]).toBe(`${authority}/oauth2/v2.0/token`);
      expect(body.get("client_id")).toBe(clientId);
      expect(body.get("client_secret")).toBe(secret);
      expect(body.get("grant_type")).toBe("client_credentials");
    }
  });

  it("keeps explicit Graph tenant selection separate from the bot token cache", async () => {
    const { app, attach, posts } = sdkWithNetwork();
    const otherTenant = "33333333-3333-4333-8333-333333333333";
    attach(otherTenant);
    await app.tokens.getBotToken();
    await app.tokens.getGraphToken(otherTenant);
    expect(posts).toHaveLength(2);
    expect(posts[1]?.url.split("?")[0]).toBe(`https://login.microsoftonline.com/${otherTenant}/oauth2/v2.0/token`);
  });

  it("propagates identity provider authentication failures instead of returning a token", async () => {
    const { app, posts } = sdkWithNetwork(true);
    await expect(app.tokens.getBotToken()).rejects.toMatchObject({ errorCode: "invalid_client" });
    expect(posts).toHaveLength(1);
  });
});

describe("GHSA-xcx6-jx8p-x3h5 regression", () => {
  function confidentialClient(nonce?: string) {
    const fixture = networkFixture(false, nonce);
    const config: Configuration = { auth: { clientId, clientSecret: secret, authority }, system: { networkClient: fixture.network } };
    return { ...fixture, config, client: new msal.ConfidentialClientApplication(config) };
  }

  it("does not send credentials or an authorization code to payload-selected hosts", async () => {
    const { client, posts } = confidentialClient();
    await client.acquireTokenByCode({ code: "trusted-request-code", scopes: [PUBLIC.graphScope], redirectUri: "https://gateway.example.test/callback" }, {
      code: "untrusted-payload-code",
      cloud_instance_host_name: "attacker.example.test",
      cloud_instance_name: "attacker-cloud",
      cloud_graph_host_name: "attacker.example.test",
      msgraph_host: "attacker.example.test",
    });
    expect(posts).toHaveLength(1);
    expect(posts[0]?.url.split("?")[0]).toBe(`${authority}/oauth2/v2.0/token`);
    expect(posts[0]?.body.get("code")).toBe("trusted-request-code");
    expect(posts[0]?.body.get("client_secret")).toBe(secret);
  });

  it("still rejects mismatched OAuth state before any token exchange", async () => {
    const { client, posts } = confidentialClient();
    await expect(client.acquireTokenByCode({ code: "code", state: "expected", scopes: [PUBLIC.graphScope], redirectUri: "https://gateway.example.test/callback" }, { code: "code", state: "different" })).rejects.toMatchObject({ errorCode: "state_mismatch" });
    expect(posts).toHaveLength(0);
  });

  it("validates the ID token nonce against the request, not the untrusted payload", async () => {
    const { client, posts } = confidentialClient("payload-nonce");
    await expect(client.acquireTokenByCode({
      code: "code", nonce: "expected-nonce", scopes: [PUBLIC.graphScope],
      redirectUri: "https://gateway.example.test/callback",
    }, { code: "code", nonce: "payload-nonce" })).rejects.toMatchObject({ errorCode: "nonce_mismatch" });
    expect(posts).toHaveLength(1);
  });

  it("rejects instance-aware confidential client configuration", () => {
    const { config } = confidentialClient();
    Object.assign(config.auth, { instanceAware: true });
    expect(() => new msal.ConfidentialClientApplication(config)).toThrow(expect.objectContaining({ errorCode: "unsupported_cca_instance_aware" }));
  });

  it.each(["true", "false", ""])("rejects instance_aware=%j authorization requests", async (value) => {
    const { client, posts } = confidentialClient();
    await expect(client.getAuthCodeUrl({ scopes: [PUBLIC.graphScope], redirectUri: "https://gateway.example.test/callback", extraQueryParameters: { instance_aware: value } })).rejects.toMatchObject({ errorCode: "unsupported_cca_instance_aware" });
    expect(posts).toHaveLength(0);
  });
});

describe("unchanged Teams inbound service-token validation", () => {
  const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
  const serviceUrl = "https://smba.trafficmanager.net/teams/";
  const keyId = "local-test-key";
  const server = createServer((_request, response) => {
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify({ keys: [{ ...publicKey.export({ format: "jwk" }), kid: keyId, use: "sig", alg: "RS256" }] }));
  });
  let validator: ServiceTokenValidator;

  beforeAll(async () => {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", resolve);
    });
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Expected local JWKS port");
    validator = new ServiceTokenValidator(clientId, tenantId, serviceUrl, logger, {
      ...PUBLIC,
      openIdMetadataUrl: `http://127.0.0.1:${address.port}/openidconfiguration`,
    });
  });

  afterAll(async () => {
    await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  });

  function signedToken(overrides: object = {}) {
    return jwt.sign({ aud: clientId, iss: PUBLIC.tokenIssuer, serviceurl: serviceUrl, exp: Math.floor(Date.now() / 1000) + 3600, ...overrides }, privateKey, { algorithm: "RS256", keyid: keyId });
  }

  it("accepts a correctly signed service token", async () => {
    const token = await validator.check(`Bearer ${signedToken()}`, { serviceUrl });
    expect(token.appId).toBe(clientId);
    expect(token.serviceUrl).toBe(serviceUrl);
  });

  it.each([
    ["audience", { aud: "another-app" }],
    ["issuer", { iss: "https://attacker.example.test" }],
    ["expiry", { exp: Math.floor(Date.now() / 1000) - 3600 }],
    ["service URL", { serviceurl: "https://attacker.example.test" }],
  ])("rejects an invalid %s", async (_label, overrides) => {
    const errors = vi.spyOn(logger, "error").mockImplementation(() => undefined);
    try {
      await expect(validator.check(`Bearer ${signedToken(overrides as object)}`, { serviceUrl })).rejects.toThrow("Invalid token");
    } finally {
      errors.mockRestore();
    }
  });

  it("rejects tampered signatures", async () => {
    const parts = signedToken().split(".");
    parts[2] = Buffer.alloc(256).toString("base64url");
    await expect(validator.check(`Bearer ${parts.join(".")}`, { serviceUrl })).rejects.toThrow("Invalid token");
  });
});
