// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * OAuth 2.1 (RFC 9700) BCP — Phase 1 conformance corpus.
 *
 * Standalone from the MCP Streamable-HTTP corpus because OAuth 2.1
 * also gates A2A inbound and AP2 transfers.
 */
import { describe, it } from "vitest";

describe("OAuth 2.1 BCP", () => {
  it.todo("PKCE S256 mandatory on public client → without PKCE → reject");
  it.todo("token endpoint rotates refresh tokens (refresh-token-rotation)");
  it.todo("authorization-code single-use enforced");
  it.todo("audience claim check rejects token for wrong audience");
  it.todo("resource indicator (RFC 8707) honored");
  it.todo("DPoP (where required) — replayed jkt rejected");
});
