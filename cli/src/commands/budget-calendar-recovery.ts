// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { createHash } from "node:crypto";
import { isDeepStrictEqual } from "node:util";

type Value = Record<string, any>;
type Execute = (args: string[], input?: string) => Promise<string>;
const annotation = "kars.azure.com/calendar-reconciliations";
const hash = (text: string): string => createHash("sha256").update(text).digest("hex");
const exact = (left: unknown, right: unknown, message: string): void => {
  if (!isDeepStrictEqual(left, right)) throw new Error(message);
};
function object(value: unknown): Value {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Malformed reconciliation evidence");
  return value as Value;
}
function integer(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) throw new Error("Inexact reconciliation amount or sequence");
  return value;
}
function identifier(value: unknown): string {
  if (typeof value !== "string" || !/^[a-zA-Z0-9][a-zA-Z0-9.-]{0,252}$/.test(value)) throw new Error("Invalid reconciliation identity");
  return value;
}
function identity(value: Value): Value {
  const meta = object(value.metadata);
  if (meta.deletionTimestamp || !meta.resourceVersion) throw new Error("Evidence is deleting or unversioned");
  return { namespace: identifier(meta.namespace), name: identifier(meta.name), uid: identifier(meta.uid) };
}

const accountBoundMode = "calendar-account-bound/v1";
const undispatchedMode = "calendar-never-reserved/v1";

function validateOrphanReceipt(receipt: Value, usage: Value): void {
  exact(receipt.pending, 1, "Orphan tombstone pending evidence changed");
  const tokens = integer(receipt.tokens);
  if (receipt.event !== "begin" && receipt.event !== "finish") throw new Error("Invalid orphan receipt event");
  if (receipt.event === "begin" && tokens !== 0) throw new Error("Unexpected Begin charge");
  if (tokens > integer(usage.monthly)) throw new Error("Original receipt charge exceeds prior calendar usage");
}

function validateFinalization(receipt: Value, audit: Value, calendar: Value, calendarUid: string): void {
  const marker = object(receipt.orphanFinalization);
  const before = { ...receipt };
  delete before.orphanFinalization;
  const undispatched = audit.version === undispatchedMode;
  if (!undispatched) exact(audit.version, accountBoundMode, "Orphan finalization audit differs");
  exact(audit.writerBefore, before, "Original orphan writer receipt differs from its audit");
  exact(audit.writerAfter, receipt, "Orphan finalization is not covered by its audit");
  exact(audit.calendarUid, calendarUid, "Orphan audit calendar UID differs");
  exact(audit.actualUsage, "unknown", "Orphan audit cannot claim measured usage");
  exact(audit.lifetimeChargeChanged, false, "Orphan audit changes lifetime charges");
  exact(audit.newFunding, false, "Orphan audit changes funding");
  exact(marker, { version: audit.version, accountUid: audit.account?.uid,
    accountResourceVersion: audit.accountResourceVersion, calendarResourceVersion: audit.beforeResourceVersion,
    lifetimeLedgerSha256: audit.lifetimeLedgerSha256, maximumTokens: audit.intent?.maximumTokens,
    day: audit.usageBefore?.day, month: audit.usageBefore?.month }, "Orphan marker proof differs");
  validateOrphanReceipt(receipt, object(audit.usageBefore));
  exact(receipt.month, marker.month, "Orphan tombstone period differs");
  const maximum = integer(marker.maximumTokens);
  if ((!undispatched && !maximum) || !/^[a-f0-9]{64}$/.test(marker.lifetimeLedgerSha256)) throw new Error("Invalid orphan accounting proof");
  if (undispatched) {
    exact(maximum, 0, "Never-reserved audit charges tokens");
    exact(receipt.tokens, 0, "Never-reserved receipt has a charge");
    const proof = object(audit.aggregateProof);
    exact(proof.retiredSessionNeverReserved, true, "Missing never-reserved proof");
    exact(proof.calendarCharge, 0, "Never-reserved proof charges tokens");
    exact(proof.allSessionsClosed, true, "Never-reserved proof has live sessions");
    exact(proof.allSessionPodsAbsent, true, "Never-reserved proof has live Pods");
    exact(proof.retiredSession?.closed, true, "Never-reserved session is not closed");
    exact(proof.retiredSession?.nextSequence, 1, "Never-reserved session accepted a reservation");
    exact(proof.retiredSession?.closedThrough, 0, "Never-reserved session has compacted history");
    exact(proof.retiredSession?.identity?.podUid, receipt.podUid, "Never-reserved session Pod differs");
    exact(proof.retiredSession?.identity?.sandbox, calendar.sandbox, "Never-reserved session Sandbox differs");
    exact(proof.retiredSession?.identity?.runtimeNamespaceUid, calendar.runtimeNamespaceUid, "Never-reserved session namespace differs");
    exact(proof.retiredSession?.identity?.taskUid, calendar.sandbox.uid, "Never-reserved session task differs");
  }
  const usage = object(audit.usageBefore);
  exact(audit.usageAfter, { ...usage, daily: integer(integer(usage.daily) + maximum), monthly: integer(integer(usage.monthly) + maximum) }, "Orphan audit does not charge its full bound");
  if (integer(calendar.usage.day) < integer(marker.day) || integer(calendar.usage.month) < integer(marker.month)
    || (calendar.usage.day === marker.day && integer(calendar.usage.daily) < audit.usageAfter.daily)
    || (calendar.usage.month === marker.month && integer(calendar.usage.monthly) < audit.usageAfter.monthly)) {
    throw new Error("Orphan finalization charge is missing from current counters");
  }
}

function accountBound(account: Value, source: Value, nsUid: string, intent: Value, pods: Value): Value {
  exact(account.metadata.annotations?.["kars.azure.com/inference-budget-bootstrap"], "sealed", "Account bootstrap is not sealed");
  const ledger = object(account.status.ledger);
  const meters = object(ledger.meters);
  exact(meters.reserved, { tokens: 0, usdMicros: 0 }, "Live reservations prevent calendar recovery");
  const nodes = object(ledger.nodes);
  exact(Object.keys(nodes), [intent.sandboxUid], "Aggregate recovery requires exactly the standalone root node");
  const node = object(nodes[intent.sandboxUid]);
  exact(node.authority?.task, source, "Standalone authority differs");
  exact(node.authority?.rootTaskUid, intent.sandboxUid, "Root task differs");
  if (node.authority?.parentUid != null) throw new Error("Shared ancestry has no standalone bound");
  exact(node.meters, meters, "Root and account cumulative meters differ");
  const sessions = object(ledger.sessions);
  const retired = object(sessions[intent.podUid]);
  exact(retired.identity?.podName, intent.podName, "Retired Pod name differs");
  const attempts = object(ledger.attempts);
  for (const [uid, entry] of Object.entries(sessions)) {
    const session = object(entry);
    exact(session.closed, true, "Every lifetime session must be closed before aggregate recovery");
    exact(session.identity?.podUid, uid, "Session Pod UID differs");
    exact(session.identity?.sandbox, source, "Session Sandbox differs");
    exact(session.identity?.runtimeNamespaceUid, nsUid, "Session runtime differs");
    exact(session.identity?.taskUid, intent.sandboxUid, "Session task differs");
    identifier(session.identity?.podName);
    if (pods.items.some((pod: Value) => pod.metadata?.uid === uid)) throw new Error("A retained lifetime session Pod still exists");
    const through = integer(session.closedThrough);
    const next = integer(session.nextSequence);
    if (next <= through || next - through > 65 || next - through - 1 > Object.keys(attempts).length) throw new Error("Session replay fence is corrupt");
    for (let sequence = through + 1; sequence < next; sequence++) {
      if (!attempts[`${uid}:${sequence}`]) throw new Error("Retained session history has a gap");
    }
  }
  const retained = { settled: { tokens: 0, usdMicros: 0 }, uncertain: { tokens: 0, usdMicros: 0 } };
  for (const [key, entry] of Object.entries(attempts)) {
    const attempt = object(entry);
    const uid = identifier(attempt.key?.podUid);
    const sequence = integer(attempt.key?.sequence);
    const session = object(sessions[uid]);
    exact(key, `${uid}:${sequence}`, "Attempt storage key differs");
    exact(attempt.identity, session.identity, "Attempt session identity differs");
    // Closed-session compaction keeps Uncertain tombstones below the fence;
    // settled/expired rows may be absent there, never above it.
    if (sequence === 0 || sequence >= session.nextSequence
      || (sequence <= session.closedThrough && !(session.closed === true && attempt.phase === "Uncertain"))) {
      throw new Error("Attempt is outside its retained replay window");
    }
    if (!["Settled", "Uncertain", "Expired"].includes(attempt.phase)) throw new Error("Nonterminal lifetime work prevents aggregate recovery");
    if (attempt.observedBreach !== undefined) throw new Error("Provider breach prevents aggregate recovery");
    if (attempt.phase === "Uncertain") exact(attempt.charged, attempt.quote?.maximum, "Uncertainty is not charged at its full maximum");
    if (attempt.phase === "Expired") exact(attempt.charged, { tokens: 0, usdMicros: 0 }, "Expired reservation has a charge");
    for (const dimension of ["tokens", "usdMicros"] as const) {
      const charged = integer(attempt.charged?.[dimension]);
      if (charged > integer(attempt.quote?.maximum?.[dimension])) throw new Error("Charge exceeds the contractual bound");
      if (attempt.phase !== "Expired") {
        const total = attempt.phase === "Uncertain" ? retained.uncertain : retained.settled;
        total[dimension] = integer(total[dimension] + charged);
      }
    }
  }
  for (const dimension of ["tokens", "usdMicros"] as const) {
    exact(retained.uncertain[dimension], integer(meters.uncertain?.[dimension]), "Uncertainty tombstones do not cover the cumulative charge");
    if (retained.settled[dimension] > integer(meters.settled?.[dimension])) throw new Error("Retained settled charges exceed cumulative meters");
  }
  // Compaction never refunds settled meters or removes uncertainty. With every
  // session closed, the entire sealed account is an upper bound for ANY orphan,
  // including one whose paired reservation was never acquired. No subtraction
  // of calendar usage or attribution to a particular attempt is justified.
  const maximum = integer(meters.settled.tokens + meters.uncertain.tokens);
  exact(intent.maximumTokens, maximum, "Authorize the entire cumulative settled-plus-uncertain account bound");
  return { formula: "settled.tokens + uncertain.tokens; no calendar subtraction", meters,
    retainedCharges: retained, retiredSession: retired, sessionCount: Object.keys(sessions).length,
    retainedAttemptCount: Object.keys(attempts).length, allSessionsClosed: true, allSessionPodsAbsent: true,
    requestCorrelation: "unavailable", compactionPreservesCumulativeCharges: true };
}

function neverReserved(account: Value, source: Value, nsUid: string, intent: Value, pods: Value): Value {
  const ledger = object(account.status.ledger);
  const meters = object(ledger.meters);
  const proof = accountBound(account, source, nsUid, {
    ...intent, maximumTokens: integer(integer(meters.settled.tokens) + integer(meters.uncertain.tokens)),
  }, pods);
  // Reservation acceptance advances nextSequence before a dispatch permit can
  // exist; compaction advances closedThrough and never rewinds either fence.
  exact(proof.retiredSession.nextSequence, 1, "Retired session accepted a reservation; conservative recovery is required");
  exact(proof.retiredSession.closedThrough, 0, "Retired session has compacted history");
  if (Object.values(object(ledger.attempts)).some(attempt => attempt.identity?.podUid === intent.podUid)) {
    throw new Error("Retired session has lifetime attempts");
  }
  return { ...proof, retiredSessionNeverReserved: true, calendarCharge: 0 };
}

/** Operator-only recovery: no router token, provider usage claim, funding or lifetime mutation. */
export async function reconcileCalendar(
  execute: Execute, authorizationText: string, apply = false, now = new Date(),
): Promise<Value> {
  const intent = object(JSON.parse(authorizationText));
  const aggregate = intent.mode === accountBoundMode;
  const undispatched = intent.mode === undispatchedMode;
  const versionPinned = aggregate || undispatched;
  exact(Object.keys(intent).sort(), ["accountUid", "calendarUid", "maximumTokens", "namespace", "sandbox", "sandboxUid", "sequence", "writer",
    ...(versionPinned ? ["mode", "accountResourceVersion", "calendarResourceVersion", "podUid", "podName"] : ["attemptSequence"])].sort(), "Unexpected reconciliation authorization fields");
  const namespace = identifier(intent.namespace);
  const target = identifier(intent.sandbox);
  for (const key of ["accountUid", "calendarUid", "sandboxUid", "writer"]) identifier(intent[key]);
  if (versionPinned) for (const key of ["accountResourceVersion", "calendarResourceVersion", "podUid", "podName"]) identifier(intent[key]);
  for (const key of ["maximumTokens", "sequence", ...(versionPinned ? [] : ["attemptSequence"])]) {
    if (undispatched && key === "maximumTokens") exact(intent[key], 0, "Never-reserved recovery cannot authorize a charge");
    else if (integer(intent[key]) === 0) throw new Error("Reconciliation bounds and sequences must be positive");
  }
  const get = async (resource: string, name: string, ns?: string): Promise<Value> => object(JSON.parse(await execute([
    "get", resource, name, ...(ns ? ["-n", ns] : []), "-o", "json",
  ])));
  const source = await get("karssandbox", target, namespace);
  const sourceIdentity = identity(source);
  exact(sourceIdentity, { namespace, name: target, uid: intent.sandboxUid }, "Sandbox incarnation changed");
  const binding = object(source.spec?.inferenceBudgetRef);
  const root = object(binding.root);
  exact(root.kind, "KarsSandbox", "Recovery supports only a standalone governed Sandbox account, not a Team/shared account");
  exact(root.resource, sourceIdentity, "Lifetime root is not this standalone Sandbox");
  exact(binding.taskUid, intent.sandboxUid, "Sandbox task binding differs");
  exact(binding.account?.uid, intent.accountUid, "Lifetime account incarnation changed");
  const account = await get("karsbudgetaccount", identifier(binding.account.name), identifier(binding.account.namespace));
  exact(identity(account), binding.account, "Lifetime account binding changed");
  exact(account.spec?.root, root, "Account root differs");
  const ledger = object(account.status?.ledger);
  exact(ledger.root, root, "Ledger root differs");
  exact(ledger.accountUid, intent.accountUid, "Ledger account UID differs");
  exact(ledger.version, "governed-inference/v1", "Unsupported lifetime ledger");
  exact(ledger.scope, "GovernedInference", "Unsupported lifetime scope");
  exact(account.spec?.scope, ledger.scope, "Account scope differs");
  exact(ledger.phase, "Active", "Frozen/closed accounts cannot be recovered by this command");
  const runtime = await get("namespace", `kars-${target}`);
  if (runtime.metadata?.deletionTimestamp) throw new Error("Runtime namespace is deleting");
  const nsUid = identifier(runtime.metadata?.uid);
  const labels = object(runtime.metadata.labels);
  exact(labels["kars.azure.com/inference-budget"], "v1", "Namespace is not governed");
  exact(labels["kars.azure.com/calendar-budget"], "v1", "Namespace has no sealed calendar");
  const annotations = object(runtime.metadata.annotations);
  exact(annotations["kars.azure.com/sandbox-namespace"], namespace, "Namespace owner workspace differs");
  exact(annotations["kars.azure.com/sandbox-name"], target, "Namespace owner name differs");
  exact(annotations["kars.azure.com/sandbox-uid"], intent.sandboxUid, "Namespace owner UID differs");
  exact(annotations["kars.azure.com/namespace-claim-version"], "v1", "Namespace ownership is not sealed");
  exact(annotations["kars.azure.com/calendar-account-uid"], intent.calendarUid, "Calendar namespace seal differs");
  const workspace = await get("namespace", namespace);
  const cluster = await get("namespace", "kube-system");
  exact(workspace.metadata?.uid, root.workspaceUid, "Workspace incarnation changed");
  exact(cluster.metadata?.uid, root.clusterUid, "Cluster incarnation changed");
  const mapName = `calendar-${hash(intent.sandboxUid)}`;
  const map = await get("configmap", mapName, binding.account.namespace);
  exact(identity(map), { namespace: binding.account.namespace, name: mapName, uid: intent.calendarUid }, "Calendar incarnation changed");
  exact(map.metadata.ownerReferences, [{ apiVersion: "v1", kind: "Namespace", name: `kars-${target}`, uid: nsUid, controller: true, blockOwnerDeletion: false }], "Calendar owner differs");
  const raw = map.data?.["ledger.json"];
  if (typeof raw !== "string" || Buffer.byteLength(raw) > 262144) throw new Error("Calendar ledger is missing/oversized");
  const calendar = object(JSON.parse(raw));
  exact(calendar.sandbox, sourceIdentity, "Calendar Sandbox binding differs");
  exact(calendar.runtimeNamespaceUid, nsUid, "Calendar runtime incarnation differs");
  if (calendar.lifetimeOnly) throw new Error("Lifetime-only enrollment has no calendar recovery");
  const audits = object(JSON.parse(map.metadata.annotations?.[annotation] ?? "{}"));
  const auditKey = `${intent.writer}:${intent.sequence}`;
  const writers = object(calendar.writers);
  if (audits[auditKey]) {
    if (!versionPinned) throw new Error("Reconciliation authorization already consumed; inspect its retained audit instead of replaying it");
    const previous = object(audits[auditKey]);
    exact(previous.version, intent.mode, "Existing reconciliation mode differs");
    exact(previous.intent, intent, "Consumed authorization differs");
    exact(writers[intent.writer], previous.writerAfter, "Finalized writer differs from its retained audit");
    validateFinalization(object(writers[intent.writer]), previous, calendar, intent.calendarUid);
    return { applied: false, alreadyApplied: true, audit: previous };
  }
  if (versionPinned) {
    exact(account.metadata.resourceVersion, intent.accountResourceVersion, "Lifetime resource version changed");
    exact(map.metadata.resourceVersion, intent.calendarResourceVersion, "Calendar resource version changed");
  }
  if (Object.keys(audits).length >= 32) throw new Error("Calendar reconciliation audit capacity reached");
  const pending = Object.entries(writers).filter(([writer, receipt]) => {
    if (versionPinned && receipt.orphanFinalization !== undefined) {
      const audit = object(audits[`${writer}:${receipt.sequence}`]);
      validateFinalization(object(receipt), audit, calendar, intent.calendarUid);
      return false;
    }
    return integer(receipt.pending) !== 0;
  });
  if (pending.length !== 1 || pending[0][0] !== intent.writer) throw new Error("Recovery requires exactly the authorized single pending writer");
  const receipt = object(pending[0][1]);
  exact(receipt.pending, 1, "Multiple pending requests have no unique recovery bound");
  exact(receipt.sequence, intent.sequence, "Calendar sequence changed");
  if (versionPinned) {
    validateOrphanReceipt(receipt, object(calendar.usage));
  } else {
    exact(receipt.event, "begin", "Expected an unfinished Begin");
    exact(receipt.tokens, 0, "Unexpected Begin charge");
  }
  const podUid = identifier(receipt.podUid);
  const pods = object(JSON.parse(await execute(["get", "pods", "-n", `kars-${target}`, "-o", "json"])));
  if (!Array.isArray(pods.items) || pods.items.some((pod: Value) => pod.metadata?.uid === podUid)) {
    throw new Error("Old writer Pod still exists; deploy the cancellation repair and retire that Pod before recovery");
  }
  let aggregateProof: Value | undefined;
  let attempt: Value | undefined;
  const meters = object(ledger.meters);
  if (versionPinned) {
    exact(podUid, intent.podUid, "Authorized retired Pod UID differs");
    if (undispatched) {
      exact(receipt.tokens, 0, "Never-reserved writer cannot have a prior charge");
      aggregateProof = neverReserved(account, sourceIdentity, nsUid, intent, pods);
    } else {
      aggregateProof = accountBound(account, sourceIdentity, nsUid, intent, pods);
    }
  } else {
    const attempts = object(ledger.attempts);
    const attemptKey = `${podUid}:${intent.attemptSequence}`;
    exact(Object.keys(attempts), [attemptKey], "Recovery requires exactly one retained lifetime attempt; ambiguous histories stay blocked");
    attempt = object(attempts[attemptKey]);
    exact(attempt.key, { podUid, sequence: intent.attemptSequence }, "Attempt key differs");
    exact(attempt.identity?.podUid, podUid, "Attempt Pod differs");
    exact(attempt.identity?.sandbox, sourceIdentity, "Attempt Sandbox differs");
    exact(attempt.identity?.runtimeNamespaceUid, nsUid, "Attempt runtime differs");
    exact(attempt.identity?.taskUid, intent.sandboxUid, "Attempt task differs");
    exact(attempt.phase, "Uncertain", "Only an already-conservatively-finalized lifetime attempt can cover recovery");
    if (attempt.observedBreach !== undefined) throw new Error("Provider breach cannot be reconciled as ordinary uncertainty");
    exact(attempt.charged, attempt.quote?.maximum, "Lifetime charge is not the acknowledged quoted maximum");
    exact(integer(attempt.charged.tokens), intent.maximumTokens, "Authorized maximum differs");
    exact(meters.reserved, { tokens: 0, usdMicros: 0 }, "Live reservations prevent calendar recovery");
    exact(meters.uncertain, attempt.charged, "Other lifetime uncertainty cannot be assigned to this calendar writer");
  }
  const day = Math.floor(now.getTime() / 86400000) + 719163;
  const month = now.getUTCFullYear() * 12 + now.getUTCMonth();
  const usageBefore = object(calendar.usage);
  if (undispatched) {
    const accountingDay = integer(usageBefore.day);
    const accountingDate = new Date((accountingDay - 719163) * 86400000);
    if (!accountingDay || accountingDay > day || !Number.isFinite(accountingDate.getTime())) throw new Error("Invalid or future calendar period");
    exact(usageBefore.month, accountingDate.getUTCFullYear() * 12 + accountingDate.getUTCMonth(), "Calendar day/month differ");
    exact(receipt.month, usageBefore.month, "Writer accounting period differs");
  } else {
    exact(usageBefore.day, day, "Cross-day reconciliation requires separate historical accounting evidence");
    exact(usageBefore.month, month, "Cross-month reconciliation requires separate historical accounting evidence");
    exact(receipt.month, month, "Writer accounting period differs");
  }
  const daily = integer(usageBefore.daily);
  const monthly = integer(usageBefore.monthly);
  if (daily > monthly) throw new Error("Calendar counters are corrupt");
  if (!versionPinned) exact(monthly, integer(meters.settled?.tokens), "Calendar total does not cover all settled lifetime usage; no inferred request correlation");
  const usageAfter = { ...usageBefore, daily: integer(daily + intent.maximumTokens), monthly: integer(monthly + intent.maximumTokens) };
  const writerAfter = versionPinned ? { ...receipt, orphanFinalization: {
    version: intent.mode, accountUid: intent.accountUid, accountResourceVersion: intent.accountResourceVersion,
    calendarResourceVersion: intent.calendarResourceVersion, lifetimeLedgerSha256: hash(JSON.stringify(ledger)),
    maximumTokens: intent.maximumTokens, day: usageBefore.day, month: usageBefore.month,
  } } : { ...receipt, sequence: integer(intent.sequence + 1), tokens: intent.maximumTokens, event: "finish", pending: 0 };
  const next = { ...calendar, usage: usageAfter, writers: { ...writers, [intent.writer]: writerAfter } };
  const encoded = JSON.stringify(next);
  if (Buffer.byteLength(encoded) > 262144) throw new Error("Recovered calendar exceeds its size bound");
  const audit = {
    version: undispatched ? undispatchedMode : aggregate ? accountBoundMode : "calendar-conservative-reconciliation/v1", intent, recordedAt: now.toISOString(),
    meaning: undispatched
      ? "Operator closure of a retired, closed lifetime session that never accepted a reservation. Not a router acknowledgement or provider usage report. Calendar periods and counters are unchanged; no existing liability is refunded."
      : "Operator conservative closure, not a router acknowledgement or provider usage report. The entire standalone lifetime total bounds this calendar; no exact request correlation is claimed.",
    actualUsage: "unknown", lifetimeChargeChanged: false, newFunding: false,
    calendarUid: intent.calendarUid, beforeResourceVersion: map.metadata.resourceVersion,
    beforeLedgerSha256: hash(raw), afterLedgerSha256: hash(encoded),
    account: binding.account, accountResourceVersion: account.metadata.resourceVersion,
    lifetimeLedgerSha256: hash(JSON.stringify(ledger)), ...(versionPinned ? { aggregateProof } : { lifetimeAttempt: attempt }),
    writerBefore: receipt, writerAfter, usageBefore, usageAfter, oldWriterPodAbsent: true,
  };
  if (versionPinned) validateFinalization(writerAfter, audit, next, intent.calendarUid);
  const nextAnnotations = { ...map.metadata.annotations, [annotation]: JSON.stringify({ ...audits, [auditKey]: audit }) };
  if (Buffer.byteLength(JSON.stringify(nextAnnotations)) > 240000) throw new Error("Recovery annotations exceed the safe Kubernetes annotation bound");
  if (!apply) return { applied: false, audit };
  // Terminal uncertainty is never refunded. Recheck all observed account state
  // before the calendar CAS; another writer's Begin would conflict with that CAS.
  const rechecked = await get("karsbudgetaccount", binding.account.name, binding.account.namespace);
  exact(identity(rechecked), binding.account, "Account identity changed before reconciliation");
  exact(rechecked.status?.ledger, ledger, "Lifetime state changed; create fresh recovery evidence");
  if (versionPinned) {
    exact(rechecked.metadata.resourceVersion, intent.accountResourceVersion, "Lifetime resource version changed before reconciliation");
    exact(rechecked.spec, account.spec, "Account specification changed before reconciliation");
    exact(rechecked.metadata.annotations, account.metadata.annotations, "Account seals changed before reconciliation");
    exact(await get("karssandbox", target, namespace), source, "Sandbox changed before reconciliation");
    exact(await get("namespace", `kars-${target}`), runtime, "Runtime namespace changed before reconciliation");
    const currentPods = object(JSON.parse(await execute(["get", "pods", "-n", `kars-${target}`, "-o", "json"])));
    if (!Array.isArray(currentPods.items)) throw new Error("Pod inventory is unavailable");
    if (undispatched) neverReserved(rechecked, sourceIdentity, nsUid, intent, currentPods);
    else accountBound(rechecked, sourceIdentity, nsUid, intent, currentPods);
  }
  const result = object(JSON.parse(await execute(["patch", "configmap", mapName, "-n", binding.account.namespace,
    "--type=merge", "--patch-file=/dev/stdin", "-o", "json"], JSON.stringify({
    metadata: { uid: intent.calendarUid, resourceVersion: map.metadata.resourceVersion, annotations: nextAnnotations },
    data: { "ledger.json": encoded },
  }))));
  exact(identity(result), identity(map), "Patched calendar incarnation differs");
  exact(result.data?.["ledger.json"], encoded, "Recovered calendar was not persisted exactly");
  exact(result.metadata.annotations?.[annotation], nextAnnotations[annotation], "Reconciliation audit was not persisted atomically");
  return { applied: true, audit };
}
