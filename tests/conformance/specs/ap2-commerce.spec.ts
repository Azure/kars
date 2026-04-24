/**
 * AP2 commerce caps — Phase 1 conformance corpus.
 *
 * Lives in the same harness as the A2A and MCP cases because AP2 is
 * carried over A2A as a tool-call extension; cap enforcement is
 * verified in concert with the JSON-RPC binding.
 *
 * Mirrors the negative class called out in
 * `docs/implementation-plan.md` §5.4 row "AP2 commerce".
 */
import { describe, it } from "vitest";

describe("AP2 commerce caps — fail-closed", () => {
  it.todo("daily cap exceeded → ToolPolicy verdict = Deny");
  it.todo("monthly cap exceeded → Deny (daily can be under)");
  it.todo("perTransfer cap exceeded → Deny even within daily/monthly");
  it.todo("counterparty not in allowlist → Deny (empty allowlist = deny-all)");
  it.todo("malformed currency string → admission CEL rejects at apply time");
  it.todo("dailyCap > monthlyCap → admission CEL rejects");
});

describe("AP2 audit trail", () => {
  it.todo("approved transfer → AuditSink receipt with signed digest");
  it.todo("denied transfer → AuditSink receipt with reason code");
  it.todo("replayed transfer (same idempotency key) → second call is no-op");
});
