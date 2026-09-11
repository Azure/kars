import assert from "node:assert/strict";
import { test } from "node:test";
import {
  credentialFormTransition, parseCredentialReview, parseCredentialContinuation,
} from "../src/lib/credential-review.ts";

const PRIVATE = "PRIVATE_VALUE_NEVER_RETURNED";
const digest = `sha256:${"a".repeat(64)}`;
const empty = () => ({ error: null, ok: null, review: null, pending: null });
const metadata = () => ({
  target: { kind: "KarsSandbox", namespace: "work", name: "target", uid: "target-uid",
    generation: 1, version: "1", intent: digest },
  grant: { uid: "grant-uid", generation: 1, version: "1", intent: digest,
    workspaceUid: "workspace-uid", legacyInventory: digest },
  source: { name: "kars-credential-input-sandbox-target", uid: null, version: null, metadataDigest: null, keys: [] },
  key: "SLACK_BOT_TOKEN",
});
const initial = () => ({ token: "header.payload.signature", expiresAt: 5000, submission: 1,
  continuation: false, bindingOnly: false, metadata: metadata() });
const receipt = stored => ({ token: "continuation.payload.signature",
  outcome: stored ? "source-stored" : "no-write-attempted",
  source: stored ? { name: metadata().source.name, uid: "source-uid", version: "1", metadataDigest: digest } : null });
function refreshed(stored) {
  const review = initial();
  review.token = "refreshed.payload.signature";
  review.submission = 2;
  review.continuation = true;
  review.bindingOnly = stored;
  review.metadata.target.version = "2";
  review.metadata.grant.version = "2";
  if (stored) review.metadata.source = { ...receipt(true).source, keys: ["SLACK_BOT_TOKEN"] };
  return review;
}
function form(operation, options = {}) {
  const data = new FormData();
  for (const [key, value] of Object.entries({
    namespace: "work", kind: "KarsSandbox", target: "target", targetUid: "target-uid",
    key: "SLACK_BOT_TOKEN", operation, ...options,
  })) data.set(key, value);
  return data;
}
function api(stored = true) {
  const calls = [];
  let writes = 0;
  return {
    calls, now: () => 100,
    review: async input => {
      calls.push(["review", input]);
      return input.continuation ? refreshed(stored) : initial();
    },
    write: async input => {
      calls.push(["write", input]);
      if (++writes === 1) throw { status: 409, code: "conflict", message: "fixed conflict", continuation: receipt(stored) };
      return { stored: true, note: "confirmed" };
    },
    failure: error => error && typeof error === "object" ? error : undefined,
  };
}

test("review has no value, confirmation gates the write, and no conflict is automatically resubmitted", async () => {
  const service = api();
  let state = await credentialFormTransition(empty(), form("review", { value: PRIVATE }), service);
  assert.equal(service.calls.length, 1);
  assert.equal("value" in service.calls[0][1], false);
  state = await credentialFormTransition(state, form("store", { value: PRIVATE }), service);
  assert.equal(service.calls.length, 1);
  state = await credentialFormTransition(state, form("store", { value: PRIVATE, confirmed: "on" }), service);
  assert.equal(service.calls.length, 2);
  assert.ok(state.pending);
  assert.equal(state.review, null);
  assert.equal(JSON.stringify(state).includes(PRIVATE), false);
  await credentialFormTransition(state, form("store", { value: PRIVATE, confirmed: "on" }), service);
  assert.equal(service.calls.length, 2);
});

for (const stored of [false, true]) {
  test(`explicit ${stored ? "acknowledged bind-only" : "confirmed no-write"} refresh requires another confirmation`, async () => {
    const service = api(stored);
    let state = await credentialFormTransition(empty(), form("review"), service);
    state = await credentialFormTransition(state, form("store", { value: PRIVATE, confirmed: "on" }), service);
    state = await credentialFormTransition(state, form("review", { value: PRIVATE }), service);
    assert.equal(state.review.bindingOnly, stored);
    assert.equal(service.calls.length, 3);
    assert.equal("value" in service.calls[2][1], false);
    await credentialFormTransition(state, form("store", { value: PRIVATE }), service);
    assert.equal(service.calls.length, 3);
    state = await credentialFormTransition(state, form("store", { value: PRIVATE, confirmed: "on" }), service);
    assert.equal(state.ok, "confirmed");
    assert.deepEqual(service.calls.map(([kind]) => kind), ["review", "write", "review", "write"]);
    assert.equal(service.calls[3][1].review, "refreshed.payload.signature");
    assert.equal(JSON.stringify(state).includes(PRIVATE), false);
  });
}

for (const change of ["uid", "generation", "intent", "grant", "grant-policy", "workspace", "source", "version", "source-intent", "expiry", "key-scope"]) {
  test(`refresh rejects changed ${change} before a second write`, async () => {
    const service = api();
    let state = await credentialFormTransition(empty(), form("review"), service);
    state = await credentialFormTransition(state, form("store", { value: PRIVATE, confirmed: "on" }), service);
    service.review = async input => {
      service.calls.push(["review", input]);
      const value = refreshed(true);
      if (change === "uid") value.metadata.target.uid = "replacement";
      if (change === "generation") value.metadata.target.generation++;
      if (change === "intent") value.metadata.target.intent = `sha256:${"b".repeat(64)}`;
      if (change === "grant") value.metadata.grant.uid = "replacement";
      if (change === "grant-policy") value.metadata.grant.intent = `sha256:${"b".repeat(64)}`;
      if (change === "workspace") value.metadata.grant.workspaceUid = "replacement";
      if (change === "source") value.metadata.source.uid = "replacement";
      if (change === "version") value.metadata.source.version = "2";
      if (change === "source-intent") value.metadata.source.metadataDigest = `sha256:${"b".repeat(64)}`;
      if (change === "expiry") value.expiresAt++;
      if (change === "key-scope") value.metadata.source.keys.push("OTHER_KEY");
      return value;
    };
    state = await credentialFormTransition(state, form("review"), service);
    assert.equal(state.review, null);
    assert.ok(state.error);
    assert.equal(service.calls.filter(([kind]) => kind === "write").length, 1);
  });
}

for (const status of [403, 409, 422, 502]) {
  test(`HTTP ${status} without a valid owned continuation cannot resume`, async () => {
    const service = api();
    service.write = async input => {
      service.calls.push(["write", input]);
      throw { status, code: status === 409 ? "conflict" : "upstream_error", message: "fixed failure" };
    };
    let state = await credentialFormTransition(empty(), form("review"), service);
    state = await credentialFormTransition(state, form("store", { value: PRIVATE, confirmed: "on" }), service);
    assert.equal(state.pending, null);
    assert.equal(state.review, null);
    assert.equal(service.calls.length, 2);
  });
}

test("metadata and previous-state projection never echoes extra secret fields", async () => {
  const untrusted = initial();
  untrusted.value = PRIVATE;
  untrusted.metadata.source.data = { token: PRIVATE };
  assert.equal(JSON.stringify(parseCredentialReview(untrusted)).includes(PRIVATE), false);
  const continuation = receipt(true);
  continuation.value = PRIVATE;
  continuation.source.data = PRIVATE;
  assert.equal(JSON.stringify(parseCredentialContinuation(continuation)).includes(PRIVATE), false);
  const state = await credentialFormTransition({ ...empty(), value: PRIVATE }, form("store"), api());
  assert.equal(JSON.stringify(state).includes(PRIVATE), false);
});

test("editing identity or submitting an expired review performs no write", async () => {
  const service = api();
  const state = { ...empty(), review: initial() };
  await credentialFormTransition(state, form("store", { target: "other", value: PRIVATE, confirmed: "on" }), service);
  assert.equal(service.calls.length, 0);
  service.now = () => 5000;
  await credentialFormTransition(state, form("store", { value: PRIVATE, confirmed: "on" }), service);
  assert.equal(service.calls.length, 0);
});
