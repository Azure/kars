// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { MAX_MISSION_ARTIFACT_BYTES, MAX_MISSION_MESSAGE_BYTES, isMissionMessage, missionContract, missionEvidenceAdvances, parseMissionContract, parseMissionMessage, snapshotMissionData, validMissionArtifacts } from "./mission-protocol.js";

import { missionInputContentDigest, missionInputsDigest } from "./mission-inputs.js";

const assignment = { type: "mission:assign", version: 1, taskName: "briefing", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid", runNonce: "run-1", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`, bootId: "boot-1", assignmentId: "assignment-1", content: "Write a briefing" };
const reply = { ...assignment, type: "mission:reply", status: "succeeded", output: "Briefing attached", evidence: { model: "model", rounds: 1, usage: { promptTokens: 1, completionTokens: 2, totalTokens: 3 } } };

describe("negotiated provisional activity", () => {
  const routerObservations = JSON.stringify({ version: 1, source: "runtime-forwarded-router-observations", coverage: "returned-responses-only",
    responses: 1, rounds: [1], state: "unavailable", reason: "Router observations unavailable after execution (reset, expiry or read failure)" });
  const progress = { ...reply, status: "running", activityFormat: "router-observations-v1", routerObservations };
  it("reserves Activity bytes without widening ordinary message or artifact budgets", () => {
    const large = routerObservations + " ".repeat(300 * 1024);
    expect(parseMissionMessage({ ...progress, routerObservations: large })).not.toBeNull();
    expect(parseMissionMessage({ ...progress, routerObservations: large, unrelated: "x".repeat(192 * 1024) })).toBeNull();
    expect(parseMissionMessage({ ...assignment, content: "x".repeat(192 * 1024) })).toBeNull();
    expect(validMissionArtifacts({ "report.md": "x".repeat(120 * 1024), "kars-router-observations.json": large })).toBe(true);
    expect(validMissionArtifacts({ "report.md": "x".repeat(128 * 1024), "kars-router-observations.json": large })).toBe(false);
    expect(validMissionArtifacts({ "kars-router-observations.json": "x".repeat(384 * 1024) })).toBe(false);
    expect(parseMissionMessage({ ...progress, routerObservations: routerObservations + " ".repeat(384 * 1024) })).toBeNull();
    expect(parseMissionMessage({ ...reply, activityFormat: "router-observations-v1", artifactFormat: "text-v1",
      artifacts: { "report.md": "x".repeat(120 * 1024), "kars-router-observations.json": large } })).not.toBeNull();
    // Base64 expansion of the maximum plaintext leaves framing headroom below
    // the maintained Python relay client's existing 1 MiB WebSocket limit.
    expect(Math.ceil(MAX_MISSION_MESSAGE_BYTES / 3) * 4).toBe(768 * 1024);
  });
  it("keeps bounded activity outside evidence and installed authority", () => {
    expect(parseMissionMessage(progress)).toMatchObject({ routerObservations, evidence: reply.evidence });
    expect(parseMissionContract({ version: 1, activityFormat: "router-observations-v1" })).toBeNull();
    expect(parseMissionMessage({ ...assignment, activityFormat: "router-observations-v1" })).not.toBeNull();
    expect(parseMissionMessage(assignment)).not.toHaveProperty("activityFormat");
  });
  it.each([undefined, "Exact-host approval is needed.", "🔥".repeat(4096), null, {}, "", " ", "🔥".repeat(4097)])("validates failed explanation bytes independently of success: %j", output => {
    const failed = { ...reply, status: "failed", error: "Required exports missing", output };
    const valid = output === undefined || (typeof output === "string" && !!output.trim() && Buffer.byteLength(output) <= 16384);
    expect(parseMissionMessage(failed) !== null).toBe(valid);
  });
  it("retains negotiated failure diagnostics without allowing failed deliverables", () => {
    expect(parseMissionMessage({ ...progress, status: "failed", output: undefined, error: "Write failed" }))
      .toMatchObject({ status: "failed", routerObservations });
    expect(parseMissionMessage({ ...progress, status: "failed", artifactFormat: "text-v1", artifacts: { "brief.md": "Not delivered" } })).toBeNull();
  });
  it("retains only the reserved bounded machine Python receipt on failure", () => {
    const failed = { ...reply, status: "failed", error: "App failed", artifactFormat: "text-v1",
      artifacts: { "kars-python-executions.json": JSON.stringify({ version: 1, runs: [{ exit_code: 1 }] }) } };
    expect(parseMissionMessage(failed)).not.toBeNull();
    expect(parseMissionMessage({ ...failed, artifactFormat: undefined })).toBeNull();
    expect(parseMissionMessage({ ...failed, artifacts: { ...failed.artifacts, "report.md": "not completed" } })).toBeNull();
    expect(parseMissionMessage({ ...failed, artifacts: { "kars-python-executions.json": "x".repeat(65537) } })).toBeNull();
  });
  it.each(["ready", "accepted", "succeeded", "rejected"])("rejects provisional bytes on %s", status => {
    expect(parseMissionMessage({ ...progress, status })).toBeNull();
  });
  it.each([undefined, "unknown"])("rejects unnegotiated activity (%s)", activityFormat => {
    expect(parseMissionMessage({ ...progress, activityFormat })).toBeNull();
  });
  it.each(["{", "null", "{}", "x".repeat(32769), JSON.stringify({ state: "observed" })])("rejects malformed or oversized snapshots (%#)", value => {
    expect(parseMissionMessage({ ...progress, routerObservations: value })).toBeNull();
  });
});

describe("native tool-correlated provisional activity", () => {
  const fixture = () => ({ version: 1, source: "runtime-forwarded-router-observations", coverage: "native-tool-correlated-only",
    correlated_tools: 1, scope_id: "scope-a", rounds: [7], state: "observed", trace: {
      scope_id: "scope-a", rounds: [7], coverage: "native-tool-correlated-only", durable: false,
      missing_rounds: [], dropped_events: 0, truncated: false, events: [
        { kind: "round", scope_id: "scope-a", round: 7, seq: 10, source: "router-upstream", provider: "foundry",
          model: "model", http_status: 200, accepted: true, outcome: "complete", usage: null, usage_state: "missing",
          finish_reason: "tool_calls", partial_observation: false, ms: 2, tool_calls_observed: 3 },
        { kind: "tool_proposed", scope_id: "scope-a", round: 7, seq: 11, source: "model-proposed",
          call_id: "write-1", name: "kars_mission_file_write", ok: null },
        { kind: "tool_result", scope_id: "scope-a", round: 7, seq: 14, source: "harness-reported",
          call_id: "write-1", name: "kars_mission_file_write", ok: true,
          result_summary: { kind: "file_write", bytes: 17, artifact_name: "briefing.md" } },
      ],
    } });
  const parse = (observations: unknown) => parseMissionMessage({ ...reply, status: "running",
    activityFormat: "router-observations-v1", routerObservations: JSON.stringify(observations) });
  it.each(["timeout", "cancelled", "output_limit", "process_limit", "launch_failed", "cleanup_failed"])(
    "retains a correlated Python %s failure without allowing a false success", reason => {
      for (const name of ["python_execute", "kars_mission_python_execute"]) {
        const observations = fixture();
        observations.trace.events[1].name = name;
        Object.assign(observations.trace.events[2], { name, ok: false, result_summary: {
          kind: "python_execution", bytes: 512, receipt_sha256: `sha256:${"a".repeat(64)}`,
          exit_code: null, reason,
        } });
        expect(parse(observations)).not.toBeNull();
        observations.trace.events[2].ok = true;
        expect(parse(observations)).toBeNull();
      }
    },
  );
  it("retains bounded argument previews without weakening native call correlation", () => {
    const observations = fixture();
    const preview = { text: '{"path":"/sandbox/report.md","content":"Useful handoff"}', truncated: false, redacted: false };
    Object.assign(observations.trace.events[1], { argument_preview: preview });
    expect(parse(observations)).toMatchObject({ routerObservations: JSON.stringify(observations) });
    for (const invalid of [null, { ...preview, text: "🦀".repeat(513) }, { ...preview, raw: "hidden" },
      { ...preview, redacted: "true" }, { ...preview, text: "" }]) {
      Object.assign(observations.trace.events[1], { argument_preview: invalid });
      expect(parse(observations)).toBeNull();
    }
  });
  it("accepts real call correlation without responses or a fabricated reporting round", () => {
    const observations = fixture();
    expect(parse(observations)).toMatchObject({ routerObservations: JSON.stringify(observations) });
    observations.trace.events.pop();
    expect(parse(observations)).not.toBeNull();
  });
  const withOperation = (extra: Record<string, unknown> = {}) => {
    const value = fixture();
    const operation = { kind: "router_operation", scope_id: "scope-a", round: 7, seq: 12,
      source: "router-policy", call_id: "write-1", name: "kars_mission_file_write", ms: 1,
      operation: "tool_policy", flag: "governance", allowed: true, http_status: null, complete: null,
      destination_host: null, destination_port: null, ...extra };
    return { ...value, trace: { ...value.trace,
      events: [...value.trace.events.slice(0, 2), operation, value.trace.events[2]] } };
  };
  it("transports correlated router decisions and fetch facts without replacing harness outcomes", () => {
    for (const extra of [
      {}, { flag: "rate_limit", allowed: false },
      { operation: "egress_policy", flag: "egress_check", destination_host: "source.example", destination_port: 443 },
      { operation: "egress_policy", flag: "private_target", allowed: false },
      { operation: "egress_fetch", source: "router", flag: "response", allowed: null, http_status: 403, complete: true },
      { operation: "egress_fetch", source: "router", flag: "body_error", allowed: null, http_status: 200, complete: false },
      { operation: "egress_fetch", source: "router", flag: "transport_error", allowed: null, complete: false },
    ]) {
      const value = withOperation(extra);
      expect(parse(value)).toMatchObject({ routerObservations: JSON.stringify(value) });
    }
  });
  it("retains the router's GitHub-service denial without accepting an allow or uncorrelated decision", () => {
    const value = withOperation({ operation: "egress_policy", flag: "github_service", allowed: false,
      destination_host: "api.github.com", destination_port: 443 });
    for (const event of value.trace.events) {
      if ("name" in event) event.name = "kars_mission_http_fetch";
      if (event.kind === "tool_result") Object.assign(event, { ok: false,
        result_summary: { kind: "failure", reason: "tool_execution_failed" } });
    }
    expect(parse(value)).not.toBeNull();
    const operation = value.trace.events[2];
    Object.assign(operation, { allowed: true });
    expect(parse(value)).toBeNull();
    Object.assign(operation, { allowed: false, call_id: "uncorrelated" });
    expect(parse(value)).toBeNull();
  });
  it("rejects uncorrelated or invented router operation fields", () => {
    for (const extra of [
      { call_id: "another" }, { name: "another" }, { round: 8 }, { seq: 10 },
      { source: "harness-reported" }, { ms: -1 }, { ms: 3_600_001 },
      { flag: "guessed-safe" }, { allowed: "yes" }, { flag: "rate_limit", allowed: true },
      { destination_host: "source.example", destination_port: null },
      { operation: "egress_policy", flag: "egress_check", destination_host: "source.example/private?key=secret", destination_port: 443 },
      { operation: "egress_policy", flag: "private_target", allowed: true },
      { operation: "egress_fetch", source: "router", flag: "response", allowed: null, http_status: 700, complete: true },
      { operation: "egress_fetch", source: "router", flag: "body_error", allowed: null, http_status: 200, complete: true },
    ]) expect(parse(withOperation(extra)), JSON.stringify(extra)).toBeNull();
  });
  it("retains OpenClaw and Hermes native file-read summaries and legacy aliases", () => {
    for (const name of ["kars_mission_file_read", "kars_mission_read", "file_read", "read_file"]) {
      const value = fixture();
      value.trace.events[1].name = name; value.trace.events[2].name = name;
      Object.assign(value.trace.events[2], { result_summary: { kind: "file_read", bytes: 17, returned_bytes: 8, truncated: true } });
      expect(parse(value)).not.toBeNull();
    }
  });
  it.each(["file_write", "write_file", "kars_mission_write", "kars_mission_file_write"])("retains bounded native %s summaries", name => {
    const value = fixture();
    value.trace.events[1].name = name; value.trace.events[2].name = name;
    expect(parse(value)).not.toBeNull();
    Object.assign(value.trace.events[2].result_summary!, { content: "unredacted file content" });
    expect(parse(value)).toBeNull();
  });
  it("accepts only the fixed native unavailable reason without response IDs", () => {
    const { trace: _trace, ...value } = fixture();
    const unavailable = { ...value, state: "unavailable", reason:
      "Native tool-correlated observations unavailable (reset, expiry, truncation or read failure)" };
    expect(parse(unavailable)).not.toBeNull();
    expect(parse({ ...unavailable, scope_id: null, rounds: [] })).not.toBeNull();
    expect(parse({ ...unavailable, reason: "Response correlation changed or was invalid" })).toBeNull();
    expect(parse({ ...unavailable, responses: null })).toBeNull();
  });
  it("rejects uncorrelated, duplicate, incomplete and fabricated coverage", () => {
    const changes: Array<(v: ReturnType<typeof fixture>) => void> = [
      v => { Object.assign(v, { responses: 1 }); },
      v => { v.correlated_tools = 0; }, v => { v.correlated_tools = 257; }, v => { v.correlated_tools = 2; },
      v => { v.trace.events[0].accepted = false; },
      v => { v.trace.events[2].call_id = "another-call"; },
      v => { v.trace.events[2].name = "another-tool"; },
      v => { v.trace.events[2].round = 8; },
      v => { Object.assign(v.trace.events[2], { reported_in_round: null }); },
      v => { Object.assign(v.trace.events[2], { reported_in_round: 7 }); },
      v => { Object.assign(v.trace.events[2], { reported_in_round: 8 }); },
      v => { v.trace.events[1].seq = 12; v.trace.events.splice(1, 0, { ...v.trace.events[2], seq: 11 }); },
      v => { v.trace.events.push({ ...v.trace.events[1], seq: 15 }); },
      v => { v.trace.events.push({ ...v.trace.events[2], seq: 15 }); },
      v => { v.trace.events.shift(); },
      v => { v.trace.events.push({ ...v.trace.events[0], seq: 15 }); },
      v => { v.trace.truncated = true; }, v => { v.trace.dropped_events = 1; },
      v => { Object.assign(v.trace, { missing_rounds: [7] }); },
      v => { v.rounds.push(8); v.trace.rounds.push(8); v.correlated_tools = 2; },
      v => { Object.assign(v.trace.events[2].result_summary!, { content: "raw contents" }); },
      v => { v.trace.scope_id = "other"; },
    ];
    for (const change of changes) { const value = fixture(); change(value); expect(parse(value), String(change)).toBeNull(); }
  });
});

describe("immutable input mission contract", () => {
  const phase = { name: "review-draft", objective: "Review the supplied immutable document", capabilities: ["filesystem-read"], minToolCalls: 1, maxToolCalls: 2 };
  const content = "Upstream draft\r\n✓ 日本語 🌍";
  const inputs = [{ name: "draft.md", content, sha256: missionInputContentDigest(content), source: {
    namespace: "kars-system", taskName: "writer", taskUid: "upstream-task", sandboxUid: "upstream-sandbox",
    podUid: "upstream-pod", runNonce: "upstream-run", assignmentId: "upstream-assignment", agentDid: assignment.agentDid, artifactName: "draft.md",
  } }];
  const contract = missionContract(phase, inputs);
  const assigned = { ...assignment, ...contract, inputArtifacts: inputs };

  it("transports a research bundle above 64KiB but still bounds the whole assignment", () => {
    const documents = [31553, 26140, 10785].map((bytes, i) => {
      const content = "x".repeat(bytes - 4) + "\n✓";
      return { ...inputs[0], name: `research-${i}.md`, content, sha256: missionInputContentDigest(content) };
    });
    const large = { ...assignment, ...missionContract(phase, documents), inputArtifacts: documents };
    expect(Buffer.byteLength(JSON.stringify(large))).toBeGreaterThan(65536);
    expect(parseMissionMessage(large)).toMatchObject({ inputArtifacts: documents });
    expect(parseMissionMessage({ ...large, content: "x".repeat(192 * 1024 - 65536) })).not.toBeNull();
    expect(parseMissionMessage({ ...large, content: "x".repeat(192 * 1024) })).toBeNull();
  });

  it("reserves independently bounded inputs without spending ordinary metadata capacity", () => {
    const documents = [63000, 63000, 63000].map((size, i) => {
      const content = "x".repeat(size);
      return { ...inputs[0], name: `memory-${i}.md`, content, sha256: missionInputContentDigest(content) };
    });
    const large = { ...assignment, ...missionContract(phase, documents), inputArtifacts: documents, content: "x".repeat(8192) };
    expect(Buffer.byteLength(JSON.stringify(documents))).toBeLessThan(192 * 1024);
    expect(Buffer.byteLength(JSON.stringify(large))).toBeGreaterThan(192 * 1024);
    expect(parseMissionMessage(large)).toMatchObject({ inputArtifacts: documents });
    expect(parseMissionMessage({ ...large, content: "x".repeat(192 * 1024) })).toBeNull();
    const oversized = [...documents, { ...documents[0], name: "extra.md" }];
    expect(parseMissionMessage({ ...large, inputArtifacts: oversized })).toBeNull();
    expect(parseMissionMessage({ ...large, type: "mission:probe", challenge: "challenge" })).toBeNull();
  });

  it("pins a canonical digest independently of assignment bytes and freezes its snapshot", () => {
    expect(contract).toMatchObject({ version: 3, inputDigest: missionInputsDigest(inputs) });
    expect(parseMissionContract(contract)).toEqual(contract);
    expect(contract).not.toHaveProperty("inputArtifacts");
    const caller = structuredClone(assigned);
    const parsed = parseMissionMessage(caller)!;
    expect(parsed).toEqual(assigned);
    caller.inputArtifacts[0].content = "changed";
    caller.inputArtifacts[0].source.runNonce = "changed";
    expect(parsed).toMatchObject({ inputArtifacts: inputs });
    if (parsed.type !== "mission:assign") throw new Error("Expected assignment");
    for (const value of [parsed.inputArtifacts, parsed.inputArtifacts![0], parsed.inputArtifacts![0].source]) expect(Object.isFrozen(value)).toBe(true);
  });

  it.each([undefined, { ...phase, capabilities: ["filesystem-write"] }, { ...phase, minToolCalls: 0, maxToolCalls: 0 }])("requires a reviewed read-capable phase with a positive ceiling (%#)", invalid => {
    expect(() => missionContract(invalid, inputs)).toThrow();
  });

  it("allows only assignments to transport input bytes, never probes or replies", () => {
    const probe = { ...assignment, ...contract, type: "mission:probe", challenge: "input-probe" };
    const ready = { ...probe, type: "mission:reply", status: "ready" };
    const success = { ...reply, ...contract, evidence: { ...reply.evidence,
      phase: { name: phase.name, minToolCalls: 1, maxToolCalls: 2, attemptedToolCalls: 1, successfulToolCalls: 1 } } };
    for (const message of [probe, ready, success]) {
      expect(parseMissionMessage(message)).not.toBeNull();
      expect(parseMissionMessage({ ...message, inputArtifacts: inputs })).toBeNull();
    }
    for (const extra of [{ inputArtifacts: inputs }, { extra: undefined }]) expect(parseMissionContract({ ...contract, ...extra })).toBeNull();
  });

  it("rejects absent inputs, digest mismatches, changed provenance and protocol downgrades", () => {
    const changed = structuredClone(inputs);
    changed[0].content += " changed"; changed[0].sha256 = missionInputContentDigest(changed[0].content);
    const rebound = structuredClone(inputs); rebound[0].source.runNonce = "another-run";
    for (const fields of [{ inputArtifacts: undefined }, { inputArtifacts: [] }, { inputDigest: undefined },
      { inputDigest: `sha256:${"0".repeat(64)}` }, { inputArtifacts: changed }, { inputArtifacts: rebound },
      { version: 1 }, { version: 2 }]) expect(parseMissionMessage({ ...assigned, ...fields })).toBeNull();
    expect(parseMissionMessage({ ...assignment, inputArtifacts: inputs })).toBeNull();
    expect(parseMissionContract({ ...contract, reviewedPhase: { ...phase, capabilities: ["filesystem-write"] } })).toBeNull();
  });
});

describe("typed required output mission contract", () => {
  const phase = { name: "synthesis", objective: "Synthesize the reviewed document", capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 1, maxToolCalls: 2, freshContext: true };
  const contract = missionContract(phase, undefined, ["report.md"]);
  const measured = { ...reply.evidence, phase: { name: phase.name, attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 2 } };
  const success = { ...reply, ...contract, artifactFormat: "text-v1", artifacts: { "report.md": "Reviewed report — 日本語" }, evidence: measured };
  it("preserves the required-output version in installed contracts, assignments and replies", () => {
    expect(contract.version).toBe(4);
    expect(parseMissionContract(contract)).toEqual(contract);
    expect(Object.isFrozen(contract.requiredOutputs)).toBe(true);
    expect(parseMissionMessage({ ...assignment, ...contract })).toMatchObject(contract);
    expect(parseMissionMessage(success)).toMatchObject({ ...contract, artifacts: success.artifacts });
    const empty = missionContract(phase, undefined, []);
    expect(parseMissionContract(empty)).toEqual({ ...empty, version: 4 });
  });
  it("sorts multiple required outputs without mutating the caller or its immutable snapshot", () => {
    const names = Object.freeze(["sources.json", "report.md"]);
    const multiple = missionContract(phase, undefined, names);
    expect(multiple.requiredOutputs).toEqual(["report.md", "sources.json"]);
    expect(names).toEqual(["sources.json", "report.md"]);
    expect(Object.isFrozen(multiple.requiredOutputs)).toBe(true);
    expect(parseMissionContract(multiple)).toEqual(multiple);
    expect(parseMissionMessage({ ...assignment, ...multiple })).toMatchObject(multiple);
    expect(parseMissionMessage({ ...success, ...multiple })).toBeNull();
    const artifacts = { ...success.artifacts, "sources.json": "{\"sources\":[]}" };
    expect(parseMissionMessage({ ...success, ...multiple, artifacts })).toMatchObject({ ...multiple, artifacts });
  });
  it("preserves required outputs when the contract also binds immutable inputs", () => {
    const content = "Successful immutable handoff";
    const inputs = [{ name: "draft.md", content, sha256: missionInputContentDigest(content), source: {
      namespace: "kars-system", taskName: "writer", taskUid: "source-task", sandboxUid: "source-sandbox",
      podUid: "source-pod", runNonce: "source-run", assignmentId: "source-assignment", agentDid: assignment.agentDid, artifactName: "handoff.md",
    } }];
    const withInputs = missionContract(phase, inputs, ["report.md"]);
    expect(parseMissionContract(withInputs)).toEqual(withInputs);
    expect(parseMissionMessage({ ...assignment, ...withInputs, inputArtifacts: inputs })).toMatchObject(withInputs);
    expect(parseMissionMessage({ ...assignment, ...withInputs })).toBeNull();
  });
  it.each([1, 2, 3])("rejects output contracts downgraded to version %s", version => {
    expect(parseMissionContract({ ...contract, version })).toBeNull();
    expect(parseMissionMessage({ ...assignment, ...contract, version })).toBeNull();
  });
  it.each([undefined, ["report.md", "report.md"], ["../report.md"], ["response.md"]])("rejects missing or invalid required names %#", requiredOutputs => {
    const value = { ...contract, requiredOutputs };
    expect(parseMissionContract(value)).toBeNull();
    expect(parseMissionMessage({ ...assignment, ...value })).toBeNull();
  });
  it.each([undefined, {}, { "other.md": "Not the required report" }, { "report.md": " " }])("rejects successful replies without the required nonblank export %#", artifacts => {
    expect(parseMissionMessage({ ...success, artifacts })).toBeNull();
    expect(parseMissionMessage({ ...success, status: "failed", artifacts: undefined })).not.toBeNull();
  });
});

describe("mandatory reviewed phase contract", () => {
  const phase = { name: "write-briefing", objective: "Write a bounded useful briefing", capabilities: ["filesystem-write", "filesystem-read"], minToolCalls: 1, maxToolCalls: 2 };
  const contract = missionContract(phase);
  const measured = { ...reply.evidence, phase: { name: phase.name, attemptedToolCalls: 1, successfulToolCalls: 1, minToolCalls: 1, maxToolCalls: 2 } };
  const success = { ...reply, ...contract, evidence: measured };
  it("canonicalizes, copies and freezes negotiated phase data without mutating the caller", () => {
    const source = structuredClone({ ...assignment, ...contract });
    source.reviewedPhase!.capabilities.reverse();
    const parsed = parseMissionMessage(source)!;
    expect(parsed).toMatchObject(contract);
    expect(source.reviewedPhase!.capabilities).toEqual(["filesystem-write", "filesystem-read"]);
    source.reviewedPhase!.maxToolCalls = 32;
    expect(parsed.reviewedPhase!.maxToolCalls).toBe(2);
    expect(Object.isFrozen(parsed)).toBe(true);
    expect(Object.isFrozen(parsed.reviewedPhase!.capabilities)).toBe(true);
    expect(parseMissionMessage(success)).not.toBeNull();
  });
  it.each([
    { version: 1, reviewedPhase: contract.reviewedPhase },
    { version: 1, phaseDigest: contract.phaseDigest },
    { version: 2 }, { version: 2, reviewedPhase: contract.reviewedPhase },
    { ...contract, phaseDigest: "sha256:" + "0".repeat(64) },
    { ...contract, reviewedPhase: { ...contract.reviewedPhase, maxToolCalls: 3 } },
  ])("rejects downgrade, omitted and changed phase contracts %#", fields => {
    expect(parseMissionMessage({ ...assignment, ...fields })).toBeNull();
  });
  it("does not allow phase evidence in legacy messages or execution evidence on probes/readiness", () => {
    expect(parseMissionMessage({ ...reply, evidence: measured })).toBeNull();
    for (const version of [missionContract(), contract]) {
      const probe = { ...assignment, ...version, type: "mission:probe", challenge: "probe" };
      expect(parseMissionMessage(probe)).not.toBeNull();
      expect(parseMissionMessage({ ...probe, evidence: measured })).toBeNull();
      expect(parseMissionMessage({ ...probe, type: "mission:reply", status: "ready", evidence: measured })).toBeNull();
    }
  });
  it.each([
    undefined, { ...measured, phase: undefined }, { ...measured, phase: { ...measured.phase, name: "different" } },
    { ...measured, phase: { ...measured.phase, attemptedToolCalls: 3 } },
    { ...measured, phase: { ...measured.phase, successfulToolCalls: 2 } },
    { ...measured, phase: { ...measured.phase, successfulToolCalls: 0 } },
    { ...measured, phase: { ...measured.phase, minToolCalls: 0 } },
    { ...measured, phase: { ...measured.phase, attemptedToolCalls: 1.5 } },
    { ...measured, model: "é".repeat(127) }, { ...measured, extra: "unbounded evidence" },
    { ...measured, usage: { ...measured.usage, extra: "unbounded usage" } },
  ])("rejects invalid successful accounting %#", evidence => {
    expect(parseMissionMessage({ ...success, evidence })).toBeNull();
  });
  it("permits bounded partial failure and running progress but not evidence-free running", () => {
    const evidence = { ...measured, phase: { ...measured.phase, successfulToolCalls: 0 } };
    expect(parseMissionMessage({ ...success, status: "running", evidence })).not.toBeNull();
    expect(parseMissionMessage({ ...success, status: "failed", evidence })).not.toBeNull();
    expect(parseMissionMessage({ ...success, status: "running", evidence: undefined })).toBeNull();
    expect(parseMissionMessage({ ...success, status: "failed", evidence: undefined })).not.toBeNull();
  });
  it("rejects erasure, counter, model, round and usage regressions", () => {
    expect(missionEvidenceAdvances(undefined, measured)).toBe(true);
    expect(missionEvidenceAdvances(measured, measured)).toBe(true);
    for (const next of [undefined, { ...measured, phase: undefined }, { ...measured, model: "different" },
      { ...measured, rounds: 0 }, { ...measured, phase: { ...measured.phase, attemptedToolCalls: 0 } },
      { ...measured, phase: { ...measured.phase, successfulToolCalls: 0 } },
      { ...measured, usage: { promptTokens: 0, completionTokens: 2, totalTokens: 2 } }]) {
      expect(missionEvidenceAdvances(measured, next)).toBe(false);
    }
    expect(missionEvidenceAdvances(measured, { ...measured, usage: null })).toBe(true);
    expect(missionEvidenceAdvances({ ...measured, usage: null }, measured)).toBe(false);
    expect(missionEvidenceAdvances({ ...measured, rounds: 0, usage: null }, measured)).toBe(true);
  });
  it("rejects nested accessors without reading them, consuming malformed reserved envelopes", () => {
    const get = vi.fn(() => "mission:assign");
    const accessor = Object.defineProperty({}, "type", { get, enumerable: true });
    expect(isMissionMessage(accessor)).toBe(true);
    expect(parseMissionMessage(accessor)).toBeNull();
    expect(parseMissionMessage({ ...success, evidence: Object.defineProperty({}, "model", { get, enumerable: true }) })).toBeNull();
    expect(get).not.toHaveBeenCalled();
    expect(parseMissionMessage(Object.create(assignment))).toBeNull();
  });
  it("bounds snapshots by depth, nodes, structure and serialized bytes", () => {
    let deep: unknown = "leaf";
    for (let i = 0; i < 18; i++) deep = { child: deep };
    for (const invalid of [deep, Array.from({ length: 16385 }, () => null), Object.assign([], { length: 1 }),
      Object.assign([], { extra: 1 }), { [Symbol("hidden")]: 1 }, Object.defineProperty({}, "hidden", { value: 1 }),
      { text: "é".repeat(MAX_MISSION_MESSAGE_BYTES) }]) expect(() => snapshotMissionData(invalid)).toThrow();
    expect(snapshotMissionData({ keep: 1, omit: undefined })).toEqual({ keep: 1 });
  });
  it("rechecks the wire bound after canonical defaults expand the phase", () => {
    const wire = { ...assignment, ...contract, reviewedPhase: phase, padding: "" };
    wire.padding = "x".repeat(MAX_MISSION_MESSAGE_BYTES - Buffer.byteLength(JSON.stringify(wire)));
    expect(Buffer.byteLength(JSON.stringify(wire))).toBe(MAX_MISSION_MESSAGE_BYTES);
    expect(parseMissionMessage(wire)).toBeNull();
  });
});

describe("installed mission contracts", () => {
  const phase = { name: "write-briefing", objective: "Write a bounded useful briefing", capabilities: ["filesystem-write", "filesystem-read"], minToolCalls: 1, maxToolCalls: 2 };
  const contract = missionContract(phase);
  it("canonicalizes an independent immutable contract without changing caller data", () => {
    const input = { ...structuredClone(contract), reviewedPhase: structuredClone(phase) };
    const parsed = parseMissionContract(input)!;
    expect(parsed).toEqual(contract);
    expect(input.reviewedPhase.capabilities).toEqual(["filesystem-write", "filesystem-read"]);
    input.reviewedPhase.maxToolCalls = 32;
    input.reviewedPhase.capabilities.length = 0;
    expect(parsed.reviewedPhase?.maxToolCalls).toBe(2);
    expect(parsed.reviewedPhase?.capabilities).toEqual(["filesystem-read", "filesystem-write"]);
    for (const value of [parsed, parsed.reviewedPhase, parsed.reviewedPhase?.capabilities, parsed.reviewedPhase?.requiredToolCalls,
      missionContract(), missionContract(phase)]) expect(Object.isFrozen(value)).toBe(true);
    expect(parseMissionContract(Object.assign(Object.create(null), contract))).toEqual(contract);
    expect(parseMissionContract({ version: 1 })).toEqual(missionContract());
  });
  it.each([undefined, null, [], {}, 1, "1", { version: 0 }, { version: 3 }, { version: "1" },
    { version: 1, phaseDigest: undefined }, { version: 1, extra: undefined },
    { version: 1, reviewedPhase: contract.reviewedPhase }, { version: 2 },
    { version: 2, reviewedPhase: contract.reviewedPhase }, { version: 2, phaseDigest: contract.phaseDigest },
    { version: 2, reviewedPhase: undefined, phaseDigest: undefined },
    { version: 3, reviewedPhase: undefined, phaseDigest: undefined, inputDigest: `sha256:${"0".repeat(64)}` },
    { version: 4, reviewedPhase: undefined, phaseDigest: undefined, requiredOutputs: [] },
    { ...contract, phaseDigest: "sha256:" + "0".repeat(64) },
    { ...contract, reviewedPhase: { ...contract.reviewedPhase, maxToolCalls: 3 } },
    { ...contract, type: "mission:probe" }, { ...contract, extra: undefined },
    { ...contract, reviewedPhase: { ...contract.reviewedPhase, objective: "é".repeat(601) } },
  ])("rejects malformed contracts and envelope fields without fallback %#", input => {
    expect(parseMissionContract(input)).toBeNull();
  });
  it("refuses getters, symbols, hidden properties and exotic objects without invoking getters", () => {
    const get = vi.fn(() => contract.reviewedPhase);
    const hidden = Object.defineProperty({ version: 1 }, "hidden", { value: undefined });
    const nested = Object.defineProperty({ ...contract.reviewedPhase }, "objective", { get, enumerable: true });
    for (const input of [Object.defineProperty({ ...contract }, "reviewedPhase", { get, enumerable: true }),
      { ...contract, reviewedPhase: nested }, { ...contract, [Symbol("extra")]: undefined }, hidden,
      Object.create(contract), new Date(), new Proxy({}, { ownKeys() { throw new Error("invalid"); } })]) {
      expect(parseMissionContract(input)).toBeNull();
    }
    expect(get).not.toHaveBeenCalled();
  });
  it("accepts the maximum supported phase contract", () => {
    const maximum = missionContract({ name: "a".repeat(48), objective: "é".repeat(600), capabilities: ["filesystem-read", "filesystem-write"], minToolCalls: 32, maxToolCalls: 32, requiredToolCalls: [], freshContext: true });
    expect(parseMissionContract(maximum)).toEqual(maximum);
  });
});

describe("bounded text mission artifacts", () => {
  it("preserves Unicode bytes in ordinary and null-prototype dictionaries", () => {
    const artifacts = { "briefing.md": "# Briefing\r\nCafé — 日本語 🌍\n" };
    expect(validMissionArtifacts(artifacts)).toBe(true);
    expect(validMissionArtifacts(Object.assign(Object.create(null), artifacts))).toBe(true);
    expect(parseMissionMessage({ ...reply, artifactFormat: "text-v1", artifacts })).toMatchObject({ artifacts });
  });
  it.each(["response.md", "__proto__", "constructor", "prototype", "../secret", "/tmp/file", ".hidden", "dir/file", "a\\b", "bad\nname", "a".repeat(129)])("rejects reserved or unsafe name %j", name => {
    expect(validMissionArtifacts(Object.fromEntries([[name, "text"]]))).toBe(false);
  });
  it.each([null, [], new Date(), new Map(), Object.create({ inherited: "text" }), { "a.md": " " }, { "a.md": 1 }, { "a.md": null }, { "a.md": 1n }])("rejects malformed dictionaries %#", value => {
    expect(validMissionArtifacts(value)).toBe(false);
  });
  it("rejects accessors, hidden keys, symbols and throwing proxies without reading content", () => {
    const get = vi.fn(() => { throw new Error("must not read"); });
    expect(validMissionArtifacts(Object.defineProperty({}, "a.md", { enumerable: true, get }))).toBe(false);
    expect(get).not.toHaveBeenCalled();
    expect(validMissionArtifacts(Object.defineProperty({}, "a.md", { value: "text" }))).toBe(false);
    expect(validMissionArtifacts({ [Symbol("file")]: "text" })).toBe(false);
    expect(validMissionArtifacts(new Proxy({}, { getPrototypeOf() { throw new Error("invalid"); } }))).toBe(false);
  });
  it("bounds the number of attached files", () => {
    const files = Object.fromEntries(Array.from({ length: 16 }, (_, i) => [`file-${i}.md`, "text"]));
    expect(validMissionArtifacts(files)).toBe(true);
    expect(validMissionArtifacts({ ...files, "extra.md": "text" })).toBe(false);
  });
  it("counts serialized UTF-8 bytes including escaping and map overhead", () => {
    const overhead = Buffer.byteLength(JSON.stringify({ "a.md": "" }));
    const exact = { "a.md": "x".repeat(MAX_MISSION_ARTIFACT_BYTES - overhead) };
    expect(validMissionArtifacts(exact)).toBe(true);
    expect(validMissionArtifacts({ "a.md": exact["a.md"] + "x" })).toBe(false);
    expect(validMissionArtifacts({ "a.md": "🌍".repeat(MAX_MISSION_ARTIFACT_BYTES / 4) })).toBe(false);
    expect(validMissionArtifacts({ "a.md": "\n".repeat(MAX_MISSION_ARTIFACT_BYTES / 2) })).toBe(false);
  });
  it("keeps old assignments and text-only replies compatible", () => {
    for (const message of [assignment, reply, { ...assignment, artifactFormat: "text-v1" }, { ...reply, artifactFormat: "text-v1" }]) expect(parseMissionMessage(message)).not.toBeNull();
    expect(parseMissionMessage({ ...assignment, artifactFormat: "text-v2" })).toBeNull();
    expect(parseMissionMessage({ ...reply, artifactFormat: "text-v2" })).toBeNull();
  });
  it.each(["ready", "accepted", "running", "failed", "rejected"])("rejects attachments on %s replies", status => {
    expect(parseMissionMessage({ ...reply, artifactFormat: "text-v1", artifacts: { "a.md": "text" }, status, challenge: "probe-1" })).toBeNull();
  });
  it("requires a negotiated success envelope and retains the whole-message bound", () => {
    const artifacts = { "a.md": "text" };
    expect(parseMissionMessage({ ...reply, artifacts })).toBeNull();
    expect(parseMissionMessage({ ...assignment, artifactFormat: "text-v1", artifacts })).toBeNull();
    expect(parseMissionMessage({ ...reply, artifactFormat: "text-v1", artifacts, output: "x".repeat(MAX_MISSION_MESSAGE_BYTES) })).toBeNull();
  });
});
