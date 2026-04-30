// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * MCP 2026-01-15 Streamable HTTP framing — Phase 1 conformance corpus.
 *
 * Ref: https://modelcontextprotocol.io/specification/2026-01-15
 *
 * **Scope.** Behavioural assertions for the router's MCP 2026
 * data plane (lands in `phase1/mcp-2026-streamable-http-routes`).
 * Captures the negative class up front so the routes PR is the
 * wiring step.
 *
 * Particular attention to OAuth 2.1 (RFC 9700) BCP — the pattern of
 * "router returns 200 but never called Content Safety" we saw in
 * production must not recur here. Every positive path has a paired
 * negative.
 */
import { describe, it } from "vitest";

describe("Streamable HTTP — happy path", () => {
  it.todo("POST /mcp with JSON-RPC 2.0 single → 200 + frame");
  it.todo("POST /mcp with batch → 200 + array of frames in order");
  it.todo("GET /mcp upgrades to streaming (chunked transfer)");
  it.todo("DELETE /mcp/<session-id> closes session and returns 204");
  it.todo("Mcp-Session-Id round-trips on subsequent calls");
});

describe("Streamable HTTP — negative", () => {
  it.todo("POST > productionMode bodyCap → 413");
  it.todo("malformed JSON-RPC → -32700");
  it.todo("method not in McpServer.spec.allowedTools → -32601");
  it.todo("scope mismatch on bearer token → 403");
  it.todo("missing Mcp-Session-Id on stateful op → 400");
  it.todo("session-id from a different McpServer → 403");
});

describe("OAuth 2.1 token verification — RFC 9700", () => {
  it.todo("missing PKCE on auth-code flow → router refuses session create");
  it.todo("expired access token → 401 with WWW-Authenticate");
  it.todo("audience mismatch → 401");
  it.todo("missing resource indicator → 401 (RFC 8707)");
  it.todo("refresh-token rotation: old refresh becomes invalid");
  it.todo("token-replay (replayed across sessions) → 401");
});

describe("Shadow-MCP detection signal", () => {
  it.todo("MCP call to host without matching McpServer CR → audit event emitted");
  it.todo("Shadow detection does not 200 if VAP is configured to block");
});
