// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * A2A 1.0.0 AgentCard JWS verification — Phase 1 conformance corpus.
 *
 * Ref:
 *   - A2A 1.0.0 spec §4.4 "Agent Card":
 *     https://a2a-protocol.org/v1.0.0/specification
 *   - RFC 7515 (JWS detached content): the AgentCardSignature shape
 *     this corpus uses follows the Rust `inference-router/src/a2a/`
 *     scaffold (agent_card.rs / signature.rs).
 *
 * **Scope of this harness.** Verifies that the router-side parser/
 * verifier (when wired in `phase1/a2a-1.0.0-routes-internal`) rejects
 * the entire negative class of cards:
 *
 *   - tampered card payload → reject (signature mismatch)
 *   - wrong issuer → reject (key thumbprint not in allowedCallers)
 *   - expired exp claim → reject
 *   - missing required field (name / protocolVersion / skills) → reject
 *   - alg=none / alg=RS256 / alg missing / non-string alg → reject
 *   - empty payload after JWS detached → reject
 *
 * Routes do not exist yet, so all cases are `.todo` until
 * `phase1/a2a-1.0.0-routes-internal` lands. The harness shape is
 * captured here so the routes PR is purely the wiring step.
 */
import { describe, it } from "vitest";

describe("AgentCard JWS — happy path", () => {
  it.todo("EdDSA-signed card with all required A2A 1.0.0 fields verifies");
  it.todo("card embeds AgentCardSignature with kid matching signing provider");
  it.todo("verified card is exposed at /.well-known/agent.json");
});

describe("AgentCard JWS — negative", () => {
  it.todo("tampered name field → signature verify fails → 401");
  it.todo("alg=none → router refuses without consulting key store");
  it.todo("alg=RS256 → router refuses (EdDSA-only allow-list)");
  it.todo("alg missing → router refuses (no implicit default)");
  it.todo("alg non-string → router refuses (parser-level)");
  it.todo("empty payload → router refuses before signature check");
  it.todo("issuer thumbprint not in spec.a2a.allowedCallers → 403");
  it.todo("exp < now → router refuses (expiry check)");
});

describe("AgentCard schema — required fields", () => {
  it.todo("missing protocolVersion → 400 with field path");
  it.todo("missing name → 400");
  it.todo("missing skills (empty array allowed) → schema-valid but advertised list empty");
  it.todo("protocolVersion != \"1.0.0\" → 400");
});

describe("AgentCard JSON-RPC binding — A2A §3", () => {
  it.todo("JSON-RPC 2.0 envelope: malformed → -32700");
  it.todo("JSON-RPC method not in advertisedSkills → -32601");
  it.todo("JSON-RPC body > spec.a2a.bodyCapBytes → 413");
});
