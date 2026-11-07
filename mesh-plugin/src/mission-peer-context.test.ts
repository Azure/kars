// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { missionContract, missionPeerContext, parseMissionContract, parseMissionContractConfiguration, parseMissionMessage, sameMissionContract } from "./mission-protocol.js";

const fixture = JSON.parse(readFileSync(new URL("../../controller/src/reviewed_execution/protocol-fixture.json", import.meta.url), "utf8"));
const phase = fixture.phaseGrant.contract.reviewedPhase;
const stateName = `kars-reviewed-${"a".repeat(40)}`;
function context() {
  return { rootUid: "reviewed-root", stateName, slot: "r0-p0", peers: [
    { role: "reviewer", phase: "review", slot: "r1-p0", agentName: `rp-${"a".repeat(20)}-r1-p0` },
    { role: "writer", phase: "draft", slot: "r0-p0", agentName: `rp-${"a".repeat(20)}-r0-p0` },
  ] };
}
const original = { type: "mission:assign", taskName: "report", taskUid: "task-uid", sandboxUid: "sandbox-uid", podUid: "pod-uid",
  runNonce: "run-1", agentDid: `did:mesh:${"a".repeat(32)}`, dispatcherDid: `did:mesh:${"b".repeat(32)}`,
  bootId: "boot-1", assignmentId: "assignment-1", content: "Exact reviewed objective", artifactFormat: "text-v1" };
const python = fileURLToPath(new URL("../../runtimes/hermes/src/kars_runtime_hermes/plugin/mission_protocol.py", import.meta.url));
function pythonContracts(values: unknown[], configured = false) {
  const result = spawnSync(process.env.KARS_TEST_PYTHON || "python3", ["-I", "-S", "-B", "-c", `
import importlib.util,json,sys
spec=importlib.util.spec_from_file_location('wire',sys.argv[1]);wire=importlib.util.module_from_spec(spec);spec.loader.exec_module(wire)
result=[]
for value in json.load(sys.stdin):
 try: result.append((wire.configured_contract if sys.argv[2] == 'configured' else wire.contract)(value))
 except (ValueError,TypeError,KeyError): result.append(None)
print(json.dumps(result))
`, python, configured ? "configured" : "contract"], { input: JSON.stringify(values), encoding: "utf8", timeout: 10000 });
  expect(result.error).toBeUndefined();
  expect(result.status, result.stderr).toBe(0);
  return JSON.parse(result.stdout);
}

describe("verified reviewed peer context", () => {
  it("canonicalizes and freezes planned locators without changing the phase", () => {
    const peers = context();
    const contract = missionContract(phase, undefined, ["handoff.md"], peers);
    expect(contract.version).toBe(5);
    expect(contract.reviewedPhase).toEqual(phase);
    expect(contract.phaseDigest).toBe(fixture.phaseGrant.contract.phaseDigest);
    expect(contract.peerContext!.peers.map(p => p.slot)).toEqual(["r0-p0", "r1-p0"]);
    expect(Object.isFrozen(contract.peerContext)).toBe(true);
    expect(Object.isFrozen(contract.peerContext!.peers)).toBe(true);
    expect(Object.isFrozen(contract.peerContext!.peers[0])).toBe(true);
    peers.peers[0].role = "changed";
    expect(contract.peerContext!.peers[1].role).toBe("reviewer");
    expect(parseMissionContract(contract)).toEqual(contract);
    expect(parseMissionMessage({ ...original, ...contract })).toMatchObject({ ...contract, content: original.content });
    expect(parseMissionMessage({ ...original, ...contract, type: "mission:reply", status: "accepted" })).toMatchObject(contract);
  });
  it("requires a reviewed phase and explicit outputs", () => {
    expect(() => missionContract(undefined, undefined, [], context())).toThrow();
    expect(() => missionContract(phase, undefined, undefined, context())).toThrow();
  });
  it("matches TypeScript and Python for v1–v5, including retained inputs", () => {
    const contracts = [missionContract(), missionContract(phase), fixture.synthesisGrant.contract,
      missionContract(phase, undefined, ["handoff.md"], context()),
      { ...fixture.synthesisGrant.contract, version: 5, peerContext: { ...context(), slot: "synthesis" } }];
    expect(pythonContracts(contracts)).toEqual(contracts.map(parseMissionContract));
  });
  it("supports all 64 phase locators and native-only synthesis", () => {
    const peers = Array.from({ length: 8 }, (_, r) => Array.from({ length: 8 }, (_, p) => ({
      role: `role-${r}-${"x".repeat(41)}`, phase: `phase-${p}-${"x".repeat(40)}`, slot: `r${r}-p${p}`, agentName: `rp-${"a".repeat(20)}-r${r}-p${p}`,
    }))).flat();
    const maximum = { ...context(), rootUid: "r".repeat(253), slot: "synthesis", peers };
    const overhead = Buffer.byteLength(JSON.stringify({ peerContext: maximum })) - 1;
    expect(overhead).toBe(11772);
    expect(8192 + overhead).toBeLessThan(32768);
    const contracts = [missionContract(phase, undefined, [], maximum),
      missionContract(phase, undefined, [], { ...context(), peers: [], slot: "synthesis" })];
    expect(pythonContracts(contracts)).toEqual(contracts);
    const configured = contracts.map(c => JSON.stringify(c));
    expect(configured.map(parseMissionContractConfiguration)).toEqual(contracts);
    expect(pythonContracts(configured, true)).toEqual(contracts);
  });
  it("bounds v5 configuration at 32 KiB while preserving legacy 8-KiB bounds in both runtimes", () => {
    const legacy = missionContract(phase, undefined, []);
    const current = missionContract(phase, undefined, [], context());
    const raw = JSON.stringify(current);
    const values = [JSON.stringify(legacy).padEnd(8192, " "), JSON.stringify(legacy).padEnd(8193, " "),
      raw.padEnd(32768, " "), raw.padEnd(32769, " "), raw.padEnd(32768, "é"), "", "null"];
    const expected = [legacy, null, current, null, null, null, null];
    expect(values.map(parseMissionContractConfiguration)).toEqual(expected);
    expect(pythonContracts(values, true)).toEqual(expected);
  });
  it("rejects malformed, ambiguous and unbounded maps in both runtimes", () => {
    const invalid = [
      { ...context(), rootUid: "bad root" }, { ...context(), stateName: "kars-reviewed-bad" },
      { ...context(), slot: "r8-p0" }, { ...context(), slot: "r0-p1" }, { ...context(), peers: [] },
      { ...context(), extra: true },
      ...[ { role: "Upper" }, { phase: "bad phase" }, { agentName: "logical-role-alias" }, { slot: "r0-p8" }, { extra: true } ]
        .map(patch => ({ ...context(), peers: [{ ...context().peers[1], ...patch }] })),
      { ...context(), peers: [context().peers[1], context().peers[1]] },
      { ...context(), peers: [context().peers[1], { ...context().peers[0], role: "writer" }] },
      { ...context(), peers: [context().peers[1], { ...context().peers[1], role: "reviewer", phase: "review", slot: "r0-p1", agentName: `rp-${"a".repeat(20)}-r0-p1` }] },
      { ...context(), peers: [context().peers[1], { ...context().peers[1], slot: "r0-p1", agentName: `rp-${"a".repeat(20)}-r0-p1` }] },
      { ...context(), peers: Array(65).fill(context().peers[1]) },
    ].map(peerContext => ({ ...fixture.phaseGrant.contract, version: 5, peerContext }));
    expect(invalid.map(parseMissionContract)).toEqual(invalid.map(() => null));
    expect(pythonContracts(invalid)).toEqual(invalid.map(() => null));
  });
  it("rejects executable, symbolic and hidden context data without invoking it", () => {
    const getter = vi.fn(() => "reviewed-root");
    const objects = [Object.defineProperty(context(), "rootUid", { get: getter, enumerable: true }),
      Object.defineProperty(context(), "hidden", { value: true }), { ...context(), [Symbol("extra")]: true }];
    for (const value of objects) expect(() => missionPeerContext(value)).toThrow();
    expect(getter).not.toHaveBeenCalled();
  });
  it("forbids peers on legacy contracts and rejects v5 omissions/downgrades", () => {
    const current = missionContract(phase, undefined, [], context());
    const invalid = [1, 2, 3, 4].map(version => ({ ...current, version }));
    const { peerContext: _peers, ...missing } = current;
    invalid.push(missing as typeof invalid[number]);
    expect(pythonContracts(invalid)).toEqual(invalid.map(() => null));
    expect(invalid.map(parseMissionContract)).toEqual(invalid.map(() => null));
    expect(parseMissionMessage({ ...original, ...current, version: 4 })).toBeNull();
  });
  it("binds root, state, own slot and peer roles in exact correlation", () => {
    const original = missionContract(phase, undefined, [], context());
    const changedState = `kars-reviewed-${"b".repeat(40)}`;
    const alternatives = [{ ...context(), rootUid: "different-root" }, { ...context(), slot: "synthesis" },
      { ...context(), peers: context().peers.map(p => ({ ...p, role: `different-${p.role}` })) },
      { ...context(), stateName: changedState, peers: context().peers.map(p => ({ ...p, agentName: `rp-${"b".repeat(20)}-${p.slot}` })) }];
    for (const peers of alternatives) expect(sameMissionContract(original, missionContract(phase, undefined, [], peers))).toBe(false);
    expect(sameMissionContract(original, missionContract(phase, undefined, [], context()))).toBe(true);
  });
});
