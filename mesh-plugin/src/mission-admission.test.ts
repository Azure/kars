// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { describe, expect, it, vi } from "vitest";
import { missionAdmissionAllows, missionObjectiveDigest, parseMissionAdmission } from "./mission-admission.js";
import { missionContentDigest } from "./mission-dispatcher.js";

const content = "\ufeff Briefing\r\nCafé — 日本語 🌍 \n";
const idle = { version: 1, state: "idle", taskGeneration: 1, authorizationDigest: `sha256:${"a".repeat(64)}` };
const run = { ...idle, state: "run", runNonce: "rev-1", objectiveDigest: missionObjectiveDigest(content) };

describe("installed mission admission", () => {
  it.each([idle, run, { ...run, taskGeneration: Number.MAX_SAFE_INTEGER, runNonce: "a".repeat(253) }])("captures immutable valid authority %#", value => {
    const captured = parseMissionAdmission(value);
    expect(captured).toEqual(value);
    expect(captured).not.toBe(value);
    expect(Object.isFrozen(captured)).toBe(true);
  });
  it.each([null, undefined, [], {}, new Date(), { ...idle, version: 2 }, { ...idle, state: "pending" },
    ...[0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, Infinity, NaN, "1", undefined].map(taskGeneration => ({ ...idle, taskGeneration })),
    ...["", "a".repeat(64), `sha256:${"A".repeat(64)}`, null].map(authorizationDigest => ({ ...idle, authorizationDigest })),
    ...["", "a".repeat(254), "-run", "run/1", "révision", "run\n"].map(runNonce => ({ ...run, runNonce })),
    ...["", `sha256:${"g".repeat(64)}`, undefined].map(objectiveDigest => ({ ...run, objectiveDigest })),
    { ...idle, runNonce: "run-1" }, { ...run, extra: undefined },
  ])("rejects malformed or over-specified authority %#", value => {
    expect(parseMissionAdmission(value)).toBeNull();
  });
  it("rejects accessors, hidden fields, symbols and custom prototypes without reading getters", () => {
    const getter = vi.fn(() => "run");
    const accessor = Object.defineProperty({ ...run }, "state", { enumerable: true, get: getter });
    const hidden = Object.defineProperty({ ...run }, "hidden", { value: true });
    const symbol = { ...run, [Symbol("extra")]: true };
    const inherited = Object.assign(Object.create({ extra: true }), run);
    for (const value of [accessor, hidden, symbol, inherited]) expect(parseMissionAdmission(value)).toBeNull();
    expect(getter).not.toHaveBeenCalled();
  });
  it("matches the dispatcher digest over exact UTF-8, without trimming or normalization", () => {
    expect(missionObjectiveDigest(content)).toBe(missionContentDigest(content));
    expect(missionObjectiveDigest(content)).not.toBe(missionObjectiveDigest(content.trim()));
    expect(missionObjectiveDigest("é")).not.toBe(missionObjectiveDigest("e\u0301"));
    expect(missionObjectiveDigest("\ud800")).toBe(missionObjectiveDigest("\ufffd"));
  });
  it("permits only the captured run and exact objective; idle permits nothing", () => {
    const input = { ...run };
    const captured = parseMissionAdmission(input)!;
    input.runNonce = "run-2"; input.objectiveDigest = missionObjectiveDigest("replacement");
    expect(missionAdmissionAllows(captured, "rev-1")).toBe(true);
    expect(missionAdmissionAllows(captured, "rev-1", content)).toBe(true);
    expect(missionAdmissionAllows(captured, "rev-1", content.trim())).toBe(false);
    expect(missionAdmissionAllows(captured, "run-2", "replacement")).toBe(false);
    expect(missionAdmissionAllows(parseMissionAdmission(idle)!, "rev-1", content)).toBe(false);
    expect(missionAdmissionAllows(parseMissionAdmission(idle)!, "rev-1")).toBe(false);
  });
});
