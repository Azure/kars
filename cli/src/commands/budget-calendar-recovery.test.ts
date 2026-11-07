// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { reconcileCalendar } from "./budget-calendar-recovery.js";
import { budgetCommand } from "./budget.js";

function fixture() {
  const resource = { namespace: "workspace", name: "sre", uid: "source-uid" };
  const root = { kind: "KarsSandbox", resource, workspaceUid: "workspace-uid", clusterUid: "cluster-uid" };
  const accountRef = { namespace: "accounting", name: "account", uid: "account-uid" };
  const metadata = (value: object) => ({ ...value, resourceVersion: "10" });
  const source = { metadata: metadata(resource), spec: { inferenceBudgetRef: { root, account: accountRef, taskUid: "source-uid" } } };
  const attempt = {
    key: { podUid: "old-pod-uid", sequence: 13 },
    identity: { podName: "old-pod", podUid: "old-pod-uid", sandbox: resource, runtimeNamespaceUid: "runtime-uid", taskUid: "source-uid" },
    charged: { tokens: 408192, usdMicros: 0 }, quote: { maximum: { tokens: 408192, usdMicros: 0 } }, phase: "Uncertain",
  };
  const account = { metadata: metadata(accountRef), spec: { root, scope: "GovernedInference" }, status: { ledger: {
    root, accountUid: "account-uid", version: "governed-inference/v1", scope: "GovernedInference", phase: "Active",
    attempts: { "old-pod-uid:13": attempt }, meters: { reserved: { tokens: 0, usdMicros: 0 },
      uncertain: { tokens: 408192, usdMicros: 0 }, settled: { tokens: 1283680, usdMicros: 0 } },
  } } };
  const runtime = { metadata: { uid: "runtime-uid", labels: { "kars.azure.com/inference-budget": "v1", "kars.azure.com/calendar-budget": "v1" },
    annotations: { "kars.azure.com/sandbox-namespace": "workspace", "kars.azure.com/sandbox-name": "sre", "kars.azure.com/sandbox-uid": "source-uid",
      "kars.azure.com/namespace-claim-version": "v1", "kars.azure.com/calendar-account-uid": "calendar-uid" } } };
  const calendar = { sandbox: resource, runtimeNamespaceUid: "runtime-uid", usage: { day: 739904, month: 24321, daily: 529504, monthly: 1283680 },
    writers: { writer: { podUid: "old-pod-uid", sequence: 45, tokens: 0, month: 24321, event: "begin", pending: 1 },
      oldWriter: { podUid: "older-pod-uid", sequence: 8, tokens: 20, month: 24321, event: "finish", pending: 0 } } };
  const mapName = `calendar-${createHash("sha256").update("source-uid").digest("hex")}`;
  const map = { metadata: { ...metadata({ namespace: "accounting", name: mapName, uid: "calendar-uid" }),
    ownerReferences: [{ apiVersion: "v1", kind: "Namespace", name: "kars-sre", uid: "runtime-uid", controller: true, blockOwnerDeletion: false }],
    annotations: { preserved: "unchanged" } }, data: { "ledger.json": "", unrelated: "unchanged" } };
  const intent = { namespace: "workspace", sandbox: "sre", sandboxUid: "source-uid", accountUid: "account-uid", calendarUid: "calendar-uid",
    writer: "writer", sequence: 45, attemptSequence: 13, maximumTokens: 408192 };
  const pods = { items: [] as Array<{ metadata: { uid: string } }> };
  const mutations: Array<{ args: string[]; input: string }> = [];
  let accountReads = 0;
  let changeAccount = false;
  let conflict = false;
  const execute = async (args: string[], input?: string): Promise<string> => {
    if (args[0] === "patch") {
      mutations.push({ args, input: input! });
      if (conflict) throw new Error("Conflict");
      const patch = JSON.parse(input!);
      expect(patch.metadata.uid).toBe("calendar-uid");
      expect(patch.metadata.resourceVersion).toBe("10");
      Object.assign(map.data, patch.data);
      Object.assign(map.metadata.annotations, patch.metadata.annotations);
      map.metadata.resourceVersion = "11";
      return JSON.stringify(map);
    }
    expect(input).toBeUndefined();
    if (args[1] === "karssandbox") return JSON.stringify(source);
    if (args[1] === "karsbudgetaccount") {
      accountReads++;
      const copy = structuredClone(account);
      if (changeAccount && accountReads > 1) copy.status.ledger.meters.settled.tokens++;
      return JSON.stringify(copy);
    }
    if (args[1] === "pods") return JSON.stringify(pods);
    if (args[1] === "namespace") return JSON.stringify(args[2] === "kars-sre" ? runtime : { metadata: { uid: `${args[2] === "workspace" ? "workspace" : "cluster"}-uid` } });
    if (args[1] === "configmap") return JSON.stringify(map);
    throw new Error(`Unexpected API operation ${args.join(" ")}`);
  };
  const run = (apply = false) => {
    if (mutations.length === 0) map.data["ledger.json"] = JSON.stringify(calendar);
    return reconcileCalendar(execute, JSON.stringify(intent), apply, new Date("2026-10-15T13:00:00Z"));
  };
  return { intent, source, root, account, runtime, calendar, map, attempt, pods, mutations, run, execute,
    changeAccount: () => { changeAccount = true; }, conflict: () => { conflict = true; } };
}

function aggregateFixture() {
  const f = fixture();
  const ledger = f.account.status.ledger as Record<string, any>;
  const intent = f.intent as Record<string, any>;
  delete intent.attemptSequence;
  Object.assign(intent, { mode: "calendar-account-bound/v1", accountResourceVersion: "10", calendarResourceVersion: "10",
    podUid: "old-pod-uid", podName: "old-pod" });
  Object.assign(f.account.metadata, { annotations: { "kars.azure.com/inference-budget-bootstrap": "sealed" } });
  const otherIdentity = { ...f.attempt.identity, podName: "older-pod", podUid: "older-pod-uid" };
  ledger.sessions = {
    "old-pod-uid": { closed: true, closedThrough: 12, nextSequence: 14, identity: f.attempt.identity },
    "older-pod-uid": { closed: true, closedThrough: 6, nextSequence: 9, identity: otherIdentity },
  };
  ledger.attempts["older-pod-uid:7"] = { key: { podUid: "older-pod-uid", sequence: 7 }, identity: otherIdentity,
    charged: { tokens: 17, usdMicros: 0 }, quote: { maximum: { tokens: 17, usdMicros: 0 } }, phase: "Uncertain" };
  ledger.attempts["older-pod-uid:8"] = { key: { podUid: "older-pod-uid", sequence: 8 }, identity: otherIdentity,
    charged: { tokens: 23, usdMicros: 0 }, quote: { maximum: { tokens: 100, usdMicros: 0 } }, phase: "Settled" };
  ledger.meters.uncertain.tokens += 17;
  ledger.meters.settled.tokens += 23;
  ledger.nodes = { "source-uid": { authority: { task: f.root.resource, rootTaskUid: "source-uid" }, meters: ledger.meters } };
  intent.maximumTokens = ledger.meters.settled.tokens + ledger.meters.uncertain.tokens;
  return { ...f, ledger, aggregateIntent: intent };
}

function neverReservedFixture() {
  const f = aggregateFixture();
  const original = f.ledger.sessions["old-pod-uid"];
  f.ledger.sessions["used-pod-uid"] = { ...original, identity: { ...original.identity, podUid: "used-pod-uid", podName: "used-pod" } };
  const attempt = f.ledger.attempts["old-pod-uid:13"];
  f.ledger.attempts["used-pod-uid:13"] = { ...attempt, key: { podUid: "used-pod-uid", sequence: 13 }, identity: f.ledger.sessions["used-pod-uid"].identity };
  delete f.ledger.attempts["old-pod-uid:13"];
  Object.assign(original, { nextSequence: 1, closedThrough: 0 });
  Object.assign(f.aggregateIntent, { mode: "calendar-never-reserved/v1", maximumTokens: 0 });
  return f;
}

describe("version-pinned never-reserved orphan finalization", () => {
  it("emits the exact shared Rust reader fixture", async () => {
    const f = neverReservedFixture();
    f.calendar.writers.writer.event = "finish";
    await f.run(true);
    const fixture = { calendarUid: f.intent.calendarUid, ledger: JSON.parse(f.map.data["ledger.json"]),
      audits: JSON.parse((f.map.metadata.annotations as Record<string, string>)["kars.azure.com/calendar-reconciliations"]) };
    await expect(JSON.stringify(fixture, null, 2) + "\n").toMatchFileSnapshot(fileURLToPath(new URL(
      "../../../shared/inference_budget/calendar-never-reserved.fixture.json", import.meta.url)));
  });

  it.each(["2026-10-15T13:00:00Z", "2026-10-16T01:00:00Z", "2026-11-01T01:00:00Z"])("preserves receipts, counters and lifetime liability across %s", async (date) => {
    const f = neverReservedFixture();
    f.calendar.writers.writer.event = "finish";
    const before = structuredClone(f.calendar);
    const accountBefore = structuredClone(f.account);
    f.map.data["ledger.json"] = JSON.stringify(f.calendar);
    const result = await reconcileCalendar(f.execute, JSON.stringify(f.intent), true, new Date(date));
    const after = JSON.parse(f.map.data["ledger.json"]);
    expect(result.audit.version).toBe("calendar-never-reserved/v1");
    expect(result.audit.aggregateProof.retiredSessionNeverReserved).toBe(true);
    expect(result.audit.aggregateProof.calendarCharge).toBe(0);
    expect(result.audit.actualUsage).toBe("unknown");
    expect(result.audit).not.toHaveProperty("lifetimeAttempt");
    expect(after.usage).toEqual(before.usage);
    expect(after.writers.writer).toEqual({ ...before.writers.writer, orphanFinalization: {
      version: "calendar-never-reserved/v1", accountUid: "account-uid", accountResourceVersion: "10", calendarResourceVersion: "10",
      lifetimeLedgerSha256: result.audit.lifetimeLedgerSha256, maximumTokens: 0, day: before.usage.day, month: before.usage.month,
    } });
    expect(after.writers.oldWriter).toEqual(before.writers.oldWriter);
    expect(f.account).toEqual(accountBefore);
    const replay = await reconcileCalendar(f.execute, JSON.stringify(f.intent), true, new Date(date));
    expect(replay).toEqual({ applied: false, alreadyApplied: true, audit: result.audit });
    expect(f.mutations).toHaveLength(1);
    expect(f.map.metadata.annotations.preserved).toBe("unchanged");
    expect(f.map.data.unrelated).toBe("unchanged");
  });

  it("is inspect-only by default", async () => {
    const f = neverReservedFixture();
    expect((await f.run()).applied).toBe(false);
    expect(f.mutations).toHaveLength(0);
  });

  for (const [name, mutate] of [
    ["accepted and compacted reservation", (f: ReturnType<typeof neverReservedFixture>) => Object.assign(f.ledger.sessions["old-pod-uid"], { nextSequence: 2, closedThrough: 1 })],
    ["retained attempt", (f: ReturnType<typeof neverReservedFixture>) => { f.ledger.attempts["old-pod-uid:13"] = f.attempt; }],
    ["unclosed session", (f: ReturnType<typeof neverReservedFixture>) => { f.ledger.sessions["old-pod-uid"].closed = false; }],
    ["other unclosed session", (f: ReturnType<typeof neverReservedFixture>) => { f.ledger.sessions["used-pod-uid"].closed = false; }],
    ["missing session", (f: ReturnType<typeof neverReservedFixture>) => { delete f.ledger.sessions["old-pod-uid"]; }],
    ["live Pod", (f: ReturnType<typeof neverReservedFixture>) => f.pods.items.push({ metadata: { uid: "old-pod-uid" } })],
    ["other live Pod", (f: ReturnType<typeof neverReservedFixture>) => f.pods.items.push({ metadata: { uid: "used-pod-uid" } })],
    ["live reservation", (f: ReturnType<typeof neverReservedFixture>) => { f.ledger.meters.reserved.tokens = 1; }],
    ["prior writer charge", (f: ReturnType<typeof neverReservedFixture>) => { Object.assign(f.calendar.writers.writer, { event: "finish", tokens: 1 }); }],
    ["nonzero authorization", (f: ReturnType<typeof neverReservedFixture>) => { f.intent.maximumTokens = 1; }],
    ["missing uncertainty", (f: ReturnType<typeof neverReservedFixture>) => { delete f.ledger.attempts["used-pod-uid:13"]; }],
    ["wrong account version", (f: ReturnType<typeof neverReservedFixture>) => { f.aggregateIntent.accountResourceVersion = "9"; }],
    ["wrong calendar version", (f: ReturnType<typeof neverReservedFixture>) => { f.aggregateIntent.calendarResourceVersion = "9"; }],
    ["multiple pending", (f: ReturnType<typeof neverReservedFixture>) => { f.calendar.writers.writer.pending = 2; }],
    ["future period", (f: ReturnType<typeof neverReservedFixture>) => { f.calendar.usage.day++; }],
    ["mismatched month", (f: ReturnType<typeof neverReservedFixture>) => { f.calendar.usage.month--; }],
    ["unsealed account", (f: ReturnType<typeof neverReservedFixture>) => { Object.assign(f.account.metadata, { annotations: {} }); }],
  ] as const) {
    it(`refuses ${name} without mutation`, async () => {
      const f = neverReservedFixture();
      mutate(f);
      await expect(f.run(true)).rejects.toThrow();
      expect(f.mutations).toHaveLength(0);
    });
  }

  it("refuses lifetime changes between proof and compare-and-swap", async () => {
    const f = neverReservedFixture();
    f.changeAccount();
    await expect(f.run(true)).rejects.toThrow("Lifetime state changed");
    expect(f.mutations).toHaveLength(0);
  });

  it("preserves the original calendar on a compare-and-swap conflict", async () => {
    const f = neverReservedFixture();
    f.conflict();
    await expect(f.run(true)).rejects.toThrow("Conflict");
    expect(JSON.parse(f.map.data["ledger.json"])).toEqual(f.calendar);
  });

  it("rejects altered never-reserved proof on replay", async () => {
    const f = neverReservedFixture();
    await f.run(true);
    const annotations = f.map.metadata.annotations as Record<string, string>;
    const audits = JSON.parse(annotations["kars.azure.com/calendar-reconciliations"]);
    audits["writer:45"].aggregateProof.retiredSession.nextSequence = 2;
    annotations["kars.azure.com/calendar-reconciliations"] = JSON.stringify(audits);
    await expect(f.run(true)).rejects.toThrow("accepted a reservation");
    expect(f.mutations).toHaveLength(1);
  });
});

describe("version-pinned whole-account orphan finalization", () => {
  it("charges the full cumulative bound without assigning a request, subtracting prior usage or inventing Finish", async () => {
    const f = aggregateFixture();
    const accountBefore = structuredClone(f.account);
    const writerBefore = structuredClone(f.calendar.writers.writer);
    const result = await f.run(true);
    expect(result.audit.version).toBe("calendar-account-bound/v1");
    expect(result.audit.aggregateProof.requestCorrelation).toBe("unavailable");
    expect(result.audit.aggregateProof.retainedAttemptCount).toBe(3);
    expect(result.audit.aggregateProof.meters.settled.tokens).toBeGreaterThan(result.audit.aggregateProof.retainedCharges.settled.tokens);
    expect(result.audit).not.toHaveProperty("lifetimeAttempt");
    const after = JSON.parse(f.map.data["ledger.json"]);
    expect(after.writers.writer).toEqual({ ...writerBefore, orphanFinalization: {
      version: "calendar-account-bound/v1", accountUid: "account-uid", accountResourceVersion: "10", calendarResourceVersion: "10",
      lifetimeLedgerSha256: result.audit.lifetimeLedgerSha256, maximumTokens: f.intent.maximumTokens, day: 739904, month: 24321,
    } });
    expect(after.usage.daily).toBe(f.calendar.usage.daily + f.intent.maximumTokens);
    expect(after.usage.monthly).toBe(f.calendar.usage.monthly + f.intent.maximumTokens);
    expect(after.writers.oldWriter).toEqual(f.calendar.writers.oldWriter);
    expect(f.account).toEqual(accountBefore);
    expect(f.map.metadata.annotations.preserved).toBe("unchanged");
    expect(f.map.data.unrelated).toBe("unchanged");
    const replay = await f.run(true);
    expect(replay).toEqual({ applied: false, alreadyApplied: true, audit: result.audit });
    expect(f.mutations).toHaveLength(1);
  });

  it.each([0, 39])("preserves a last Finish charge of %i with one remaining orphan and compacted uncertainty", async (tokens) => {
    const f = aggregateFixture();
    Object.assign(f.calendar.writers.writer, { sequence: 7, event: "finish", tokens });
    f.intent.sequence = 7;
    f.ledger.sessions["old-pod-uid"].closedThrough = 13;
    f.ledger.sessions["older-pod-uid"].closedThrough = 8;
    delete f.ledger.attempts["older-pod-uid:8"];
    const accountBefore = structuredClone(f.account);
    const writerBefore = structuredClone(f.calendar.writers.writer);
    const result = await f.run(true);
    const after = JSON.parse(f.map.data["ledger.json"]);
    expect(result.audit.writerBefore).toEqual(writerBefore);
    expect(result.audit.aggregateProof.requestCorrelation).toBe("unavailable");
    expect(result.audit.aggregateProof.retainedAttemptCount).toBe(2);
    expect(after.writers.writer).toEqual({ ...writerBefore, orphanFinalization: result.audit.writerAfter.orphanFinalization });
    expect(after.usage).toEqual({ ...f.calendar.usage,
      daily: f.calendar.usage.daily + f.intent.maximumTokens, monthly: f.calendar.usage.monthly + f.intent.maximumTokens });
    expect(f.account).toEqual(accountBefore);
    expect(await f.run(true)).toEqual({ applied: false, alreadyApplied: true, audit: result.audit });
    expect(f.mutations).toHaveLength(1);
  });

  it("keeps all 24 below-watermark uncertainty tombstones and cumulative settled charges", async () => {
    const f = aggregateFixture();
    f.calendar.writers.writer.event = "finish";
    f.calendar.writers.writer.sequence = f.intent.sequence = 7;
    f.ledger.attempts = Object.fromEntries(Array.from({ length: 24 }, (_, index) => {
      const attempt = structuredClone(f.attempt);
      attempt.key.sequence = index + 1;
      return [`old-pod-uid:${index + 1}`, attempt];
    }));
    Object.assign(f.ledger.sessions["old-pod-uid"], { closedThrough: 24, nextSequence: 25 });
    f.ledger.sessions["older-pod-uid"].closedThrough = 8;
    f.ledger.meters.uncertain.tokens = 24 * f.attempt.charged.tokens;
    f.intent.maximumTokens = f.ledger.meters.settled.tokens + f.ledger.meters.uncertain.tokens;
    const before = structuredClone(f.account);
    const result = await f.run(true);
    expect(result.audit.aggregateProof.retainedAttemptCount).toBe(24);
    expect(result.audit.aggregateProof.retainedCharges.uncertain).toEqual(f.ledger.meters.uncertain);
    expect(result.audit.aggregateProof.retainedCharges.settled.tokens).toBe(0);
    expect(result.audit.usageAfter.monthly).toBe(f.calendar.usage.monthly + f.intent.maximumTokens);
    expect(f.account).toEqual(before);
  });

  it("preserves a previously finalized orphan and its audit during a subsequent Finish-orphan recovery", async () => {
    const f = aggregateFixture();
    const first = await f.run(true);
    const calendar = JSON.parse(f.map.data["ledger.json"]);
    const priorWriter = structuredClone(calendar.writers.writer);
    Object.assign(calendar.writers.oldWriter, { sequence: 7, event: "finish", pending: 1, tokens: 20 });
    f.map.data["ledger.json"] = JSON.stringify(calendar);
    Object.assign(f.intent, { writer: "oldWriter", sequence: 7, podUid: "older-pod-uid", podName: "older-pod", calendarResourceVersion: "11" });
    const execute = async (args: string[], input?: string) => {
      if (args[0] !== "patch") return f.execute(args, input);
      const patch = JSON.parse(input!);
      expect(patch.metadata.resourceVersion).toBe("11");
      Object.assign(f.map.data, patch.data);
      Object.assign(f.map.metadata.annotations, patch.metadata.annotations);
      f.map.metadata.resourceVersion = "12";
      return JSON.stringify(f.map);
    };
    const result = await reconcileCalendar(execute, JSON.stringify(f.intent), true, new Date("2026-10-15T13:00:00Z"));
    const after = JSON.parse(f.map.data["ledger.json"]);
    const audits = JSON.parse((f.map.metadata.annotations as Record<string, string>)["kars.azure.com/calendar-reconciliations"]);
    expect(after.writers.writer).toEqual(priorWriter);
    expect(audits["writer:45"]).toEqual(first.audit);
    expect(audits["oldWriter:7"]).toEqual(result.audit);
    expect(result.audit.writerBefore).toEqual(calendar.writers.oldWriter);
    expect(after.usage.monthly).toBe(calendar.usage.monthly + f.intent.maximumTokens);
  });

  it("is inspect-only by default", async () => {
    const f = aggregateFixture();
    expect((await f.run()).applied).toBe(false);
    expect(f.mutations).toHaveLength(0);
  });

  for (const [title, mutate] of [
    ["missing retired session", (f: ReturnType<typeof aggregateFixture>) => { delete f.ledger.sessions["old-pod-uid"]; }],
    ["wrong retired Pod UID", (f: ReturnType<typeof aggregateFixture>) => { f.aggregateIntent.podUid = "different"; }],
    ["wrong retired Pod name", (f: ReturnType<typeof aggregateFixture>) => { f.aggregateIntent.podName = "different"; }],
    ["unsealed bootstrap", (f: ReturnType<typeof aggregateFixture>) => { Object.assign(f.account.metadata, { annotations: {} }); }],
    ["retired session still open", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.sessions["old-pod-uid"].closed = false; }],
    ["other session still open", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.sessions["older-pod-uid"].closed = false; }],
    ["old Pod still present", (f: ReturnType<typeof aggregateFixture>) => { f.pods.items.push({ metadata: { uid: "old-pod-uid" } }); }],
    ["other session Pod still present", (f: ReturnType<typeof aggregateFixture>) => { f.pods.items.push({ metadata: { uid: "older-pod-uid" } }); }],
    ["wrong session runtime", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.sessions["older-pod-uid"].identity.runtimeNamespaceUid = "different"; }],
    ["session gap", (f: ReturnType<typeof aggregateFixture>) => { delete f.ledger.attempts["older-pod-uid:8"]; }],
    ["corrupt replay window", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.sessions["old-pod-uid"].closedThrough = 14; }],
    ["settled attempt below fence", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.sessions["older-pod-uid"].closedThrough = 8; }],
    ["expired attempt below fence", (f: ReturnType<typeof aggregateFixture>) => {
      f.ledger.sessions["older-pod-uid"].closedThrough = 8;
      Object.assign(f.ledger.attempts["older-pod-uid:8"], { phase: "Expired", charged: { tokens: 0, usdMicros: 0 } });
    }],
    ["zero sequence tombstone", (f: ReturnType<typeof aggregateFixture>) => {
      f.attempt.key.sequence = 0; delete f.ledger.attempts["old-pod-uid:13"]; f.ledger.attempts["old-pod-uid:0"] = f.attempt;
      f.ledger.sessions["old-pod-uid"].closedThrough = 13;
    }],
    ["attempt at next sequence", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.sessions["old-pod-uid"].nextSequence = 13; }],
    ["compacted uncertainty missing", (f: ReturnType<typeof aggregateFixture>) => {
      f.ledger.sessions["old-pod-uid"].closedThrough = 13; delete f.ledger.attempts["old-pod-uid:13"];
    }],
    ["invalid last event", (f: ReturnType<typeof aggregateFixture>) => { f.calendar.writers.writer.event = "unknown"; }],
    ["charged Begin", (f: ReturnType<typeof aggregateFixture>) => { f.calendar.writers.writer.tokens = 1; }],
    ["Finish charge absent from prior usage", (f: ReturnType<typeof aggregateFixture>) => {
      Object.assign(f.calendar.writers.writer, { event: "finish", tokens: f.calendar.usage.monthly + 1 });
    }],
    ["inexact Finish charge", (f: ReturnType<typeof aggregateFixture>) => { Object.assign(f.calendar.writers.writer, { event: "finish", tokens: 0.5 }); }],
    ["in-flight work", (f: ReturnType<typeof aggregateFixture>) => { f.attempt.phase = "InFlight"; }],
    ["reservation", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.meters.reserved.tokens = 1; }],
    ["breach evidence", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.attempts["older-pod-uid:8"].observedBreach = "observed"; }],
    ["nonmaximum uncertainty", (f: ReturnType<typeof aggregateFixture>) => { f.attempt.charged.tokens--; }],
    ["missing uncertainty tombstone", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.meters.uncertain.tokens++; }],
    ["understated settled meters", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.meters.settled.tokens = 22; }],
    ["root meter mismatch", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.nodes["source-uid"].meters = { ...f.ledger.meters, settled: { tokens: 0, usdMicros: 0 } }; }],
    ["other account node", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.nodes.other = f.ledger.nodes["source-uid"]; }],
    ["shared root", (f: ReturnType<typeof aggregateFixture>) => { f.root.kind = "KarsTeam"; }],
    ["frozen account", (f: ReturnType<typeof aggregateFixture>) => { f.ledger.phase = "Frozen"; }],
    ["stale account version", (f: ReturnType<typeof aggregateFixture>) => { f.aggregateIntent.accountResourceVersion = "9"; }],
    ["stale calendar version", (f: ReturnType<typeof aggregateFixture>) => { f.aggregateIntent.calendarResourceVersion = "9"; }],
    ["guessed request maximum", (f: ReturnType<typeof aggregateFixture>) => { f.intent.maximumTokens = 408192; }],
    ["discounted calendar coverage", (f: ReturnType<typeof aggregateFixture>) => { f.intent.maximumTokens -= f.calendar.usage.monthly; }],
    ["multiple pending requests", (f: ReturnType<typeof aggregateFixture>) => { f.calendar.writers.writer.pending = 2; }],
    ["another pending writer", (f: ReturnType<typeof aggregateFixture>) => { f.calendar.writers.oldWriter.pending = 1; }],
    ["cross-day accounting", (f: ReturnType<typeof aggregateFixture>) => { f.calendar.usage.day--; }],
    ["cross-month accounting", (f: ReturnType<typeof aggregateFixture>) => { f.calendar.usage.month--; }],
    ["counter overflow", (f: ReturnType<typeof aggregateFixture>) => { f.calendar.usage.monthly = Number.MAX_SAFE_INTEGER; }],
    ["unaudited orphan marker", (f: ReturnType<typeof aggregateFixture>) => { Object.assign(f.calendar.writers.writer, { orphanFinalization: {} }); }],
  ] as const) {
    it(`rejects ${title} without mutation`, async () => {
      const f = aggregateFixture(); mutate(f);
      await expect(f.run(true)).rejects.toThrow();
      expect(f.mutations).toHaveLength(0);
    });
  }

  it("does not double-charge after the server persists a patch but the client loses its acknowledgement", async () => {
    const f = aggregateFixture();
    f.map.data["ledger.json"] = JSON.stringify(f.calendar);
    const execute = async (args: string[], input?: string) => {
      const result = await f.execute(args, input);
      if (args[0] === "patch") throw new Error("Lost acknowledgement");
      return result;
    };
    await expect(reconcileCalendar(execute, JSON.stringify(f.intent), true, new Date("2026-10-15T13:00:00Z")))
      .rejects.toThrow("Lost acknowledgement");
    expect((await f.run(true)).alreadyApplied).toBe(true);
    expect(f.mutations).toHaveLength(1);
  });

  it("rejects a changed authorization on replay", async () => {
    const f = aggregateFixture();
    await f.run(true);
    f.intent.maximumTokens--;
    await expect(f.run(true)).rejects.toThrow("Consumed authorization differs");
    expect(f.mutations).toHaveLength(1);
  });

  it("rejects a finalization changed after an unconfirmed write", async () => {
    const f = aggregateFixture();
    await f.run(true);
    const changed = JSON.parse(f.map.data["ledger.json"]);
    changed.writers.writer.pending = 0;
    f.map.data["ledger.json"] = JSON.stringify(changed);
    await expect(f.run(true)).rejects.toThrow("Finalized writer differs");
    expect(f.mutations).toHaveLength(1);
  });

  it("rejects an audited Finish charge not covered by the pre-recovery counters", async () => {
    const f = aggregateFixture();
    f.calendar.writers.writer.event = "finish";
    await f.run(true);
    const changed = JSON.parse(f.map.data["ledger.json"]);
    const annotations = f.map.metadata.annotations as Record<string, string>;
    const audits = JSON.parse(annotations["kars.azure.com/calendar-reconciliations"]);
    changed.writers.writer.tokens = f.calendar.usage.monthly + 1;
    audits["writer:45"].writerBefore.tokens = changed.writers.writer.tokens;
    audits["writer:45"].writerAfter.tokens = changed.writers.writer.tokens;
    annotations["kars.azure.com/calendar-reconciliations"] = JSON.stringify(audits);
    f.map.data["ledger.json"] = JSON.stringify(changed);
    await expect(f.run(true)).rejects.toThrow("Original receipt charge exceeds prior calendar usage");
    expect(f.mutations).toHaveLength(1);
  });

  it("rejects replay when the conservative charge was removed", async () => {
    const f = aggregateFixture();
    await f.run(true);
    const changed = JSON.parse(f.map.data["ledger.json"]);
    changed.usage.monthly--;
    f.map.data["ledger.json"] = JSON.stringify(changed);
    await expect(f.run(true)).rejects.toThrow("charge is missing from current counters");
    expect(f.mutations).toHaveLength(1);
  });

  it("rejects concurrent account movement and calendar CAS conflicts", async () => {
    const f = aggregateFixture(); f.changeAccount();
    await expect(f.run(true)).rejects.toThrow("Lifetime state changed");
    expect(f.mutations).toHaveLength(0);
    const conflict = aggregateFixture(); conflict.conflict();
    await expect(conflict.run(true)).rejects.toThrow("Conflict");
    expect(conflict.mutations).toHaveLength(1);
  });

  it("checks the account resource version even if its ledger content is unchanged", async () => {
    const f = aggregateFixture();
    f.map.data["ledger.json"] = JSON.stringify(f.calendar);
    let reads = 0;
    const execute = async (args: string[], input?: string) => {
      const result = await f.execute(args, input);
      if (args[1] === "karsbudgetaccount" && ++reads > 1) {
        const value = JSON.parse(result); value.metadata.resourceVersion = "11"; return JSON.stringify(value);
      }
      return result;
    };
    await expect(reconcileCalendar(execute, JSON.stringify(f.intent), true, new Date("2026-10-15T13:00:00Z")))
      .rejects.toThrow("Lifetime resource version changed before reconciliation");
    expect(f.mutations).toHaveLength(0);
  });
});

describe("operator conservative calendar reconciliation", () => {
  it("is registered on the maintained budget CLI and defaults to inspection", async () => {
    const command = budgetCommand().commands.find((entry) => entry.name() === "reconcile-calendar");
    expect(command).toBeDefined();
    expect(command!.options.find((entry) => entry.long === "--apply")!.defaultValue).toBe(false);
    const f = fixture();
    const result = await f.run();
    expect(result.applied).toBe(false);
    expect(result.audit.actualUsage).toBe("unknown");
    expect(result.audit.lifetimeChargeChanged).toBe(false);
    expect(result.audit.usageAfter).toMatchObject({ daily: 937696, monthly: 1691872 });
    expect(f.mutations).toHaveLength(0);
  });

  it("atomically persists bounded closure and reconstructible original evidence, without changing lifetime accounting", async () => {
    const f = fixture();
    const originalAccount = structuredClone(f.account);
    const before = structuredClone(f.calendar);
    const result = await f.run(true);
    expect(result.applied).toBe(true);
    expect(f.mutations).toHaveLength(1);
    expect(f.mutations[0].args).toContain("--patch-file=/dev/stdin");
    expect(f.account).toEqual(originalAccount);
    expect(result.audit.writerBefore).toEqual(before.writers.writer);
    expect(result.audit.usageBefore).toEqual(before.usage);
    expect(result.audit.lifetimeAttempt).toEqual(f.attempt);
    const next = JSON.parse(f.map.data["ledger.json"]);
    expect(next.writers.writer).toMatchObject({ sequence: 46, event: "finish", pending: 0, tokens: 408192 });
    expect(next.writers.oldWriter).toEqual(before.writers.oldWriter);
    expect(f.map.data.unrelated).toBe("unchanged");
    expect(f.map.metadata.annotations.preserved).toBe("unchanged");
    await expect(f.run(true)).rejects.toThrow("already consumed");
    expect(f.mutations).toHaveLength(1);
  });

  for (const [title, mutate] of [
    ["live old Pod, even if unready", (f: ReturnType<typeof fixture>) => f.pods.items.push({ metadata: { uid: "old-pod-uid" } })],
    ["multiple calendar requests", (f: ReturnType<typeof fixture>) => { f.calendar.writers.writer.pending = 2; }],
    ["last Finish cannot establish one-attempt attribution", (f: ReturnType<typeof fixture>) => { f.calendar.writers.writer.event = "finish"; }],
    ["another pending writer", (f: ReturnType<typeof fixture>) => { f.calendar.writers.oldWriter.pending = 1; }],
    ["stale writer sequence", (f: ReturnType<typeof fixture>) => { f.intent.sequence++; }],
    ["wrong maximum", (f: ReturnType<typeof fixture>) => { f.intent.maximumTokens--; }],
    ["nonmaximum terminal charge", (f: ReturnType<typeof fixture>) => { f.attempt.charged.tokens--; }],
    ["nonterminal lifetime attempt", (f: ReturnType<typeof fixture>) => { f.attempt.phase = "InFlight"; }],
    ["another uncertain charge", (f: ReturnType<typeof fixture>) => { f.account.status.ledger.meters.uncertain.tokens++; }],
    ["live reservation", (f: ReturnType<typeof fixture>) => { f.account.status.ledger.meters.reserved.tokens++; }],
    ["missing settled calendar coverage", (f: ReturnType<typeof fixture>) => { f.calendar.usage.monthly--; }],
    ["cross-day history", (f: ReturnType<typeof fixture>) => { f.calendar.usage.day--; }],
    ["cross-month history", (f: ReturnType<typeof fixture>) => { f.calendar.usage.month--; }],
    ["shared Team account", (f: ReturnType<typeof fixture>) => { f.root.kind = "KarsTeam"; }],
    ["changed Sandbox UID", (f: ReturnType<typeof fixture>) => { f.intent.sandboxUid = "different"; }],
    ["changed lifetime UID", (f: ReturnType<typeof fixture>) => { f.intent.accountUid = "different"; }],
    ["changed calendar UID", (f: ReturnType<typeof fixture>) => { f.intent.calendarUid = "different"; }],
    ["changed runtime UID", (f: ReturnType<typeof fixture>) => { f.runtime.metadata.uid = "different"; }],
    ["wrong namespace ownership", (f: ReturnType<typeof fixture>) => { f.runtime.metadata.annotations["kars.azure.com/sandbox-uid"] = "different"; }],
    ["wrong attempt Sandbox", (f: ReturnType<typeof fixture>) => { f.attempt.identity = { ...f.attempt.identity, sandbox: { ...f.attempt.identity.sandbox, uid: "different" } }; }],
    ["frozen account", (f: ReturnType<typeof fixture>) => { f.account.status.ledger.phase = "Frozen"; }],
    ["inexact amount", (f: ReturnType<typeof fixture>) => { f.intent.maximumTokens = Number.MAX_SAFE_INTEGER + 1; }],
  ] as const) {
    it(`rejects ${title} without mutation`, async () => {
      const f = fixture(); mutate(f);
      await expect(f.run(true)).rejects.toThrow();
      expect(f.mutations).toHaveLength(0);
    });
  }

  it("rejects concurrent lifetime movement rather than using a stale proof", async () => {
    const f = fixture(); f.changeAccount();
    await expect(f.run(true)).rejects.toThrow("Lifetime state changed");
    expect(f.mutations).toHaveLength(0);
  });
  it("does not retry a failed calendar CAS or issue any account mutation", async () => {
    const f = fixture(); f.conflict();
    await expect(f.run(true)).rejects.toThrow("Conflict");
    expect(f.mutations).toHaveLength(1);
    expect(f.mutations[0].args.slice(0, 2)).toEqual(["patch", "configmap"]);
  });
});
