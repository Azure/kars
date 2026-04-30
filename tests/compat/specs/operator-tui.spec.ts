// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/**
 * Compat spec: `azureclaw operator` (headless TUI).
 *
 * Flow: internal Phase 1 plan §5.1 #6.
 *
 * Phase 0 scope (this file): harness self-tests + protected-flow catalogue
 * assertions. These verify the compat harness itself is wired correctly,
 * so subsequent Phase 1 specs can rely on it.
 *
 * Phase 1 scope (incoming): render the actual operator TUI against this
 * harness with kubectl-mock fixtures, snapshot the screen, drive keyboard
 * navigation, assert the state machine.
 */

import { describe, expect, it } from "vitest";
import { PROTECTED_FLOWS, type ProtectedFlow } from "../harness/types.js";
import { MockNode, MockScreen, blessedMock, blessedContribMock } from "../harness/blessed-mock.js";

describe("compat: harness sanity", () => {
  it("blessedMock.screen() returns a MockScreen with renderCount=0", () => {
    const screen = blessedMock.screen({ title: "test" });
    expect(screen).toBeInstanceOf(MockScreen);
    expect(screen.renderCount).toBe(0);
  });

  it("render() increments renderCount", () => {
    const screen = blessedMock.screen();
    screen.render();
    screen.render();
    expect(screen.renderCount).toBe(2);
  });

  it("blessedMock.box() with parent attaches to the screen tree", () => {
    const screen = blessedMock.screen();
    const box = blessedMock.box({ parent: screen, label: "Agents", content: "foo" });
    expect(box.parent).toBe(screen);
    expect(screen.children).toContain(box);
    expect(box.getContent()).toBe("foo");
    expect(box.kind).toBe("box");
  });

  it("setContent + findByKind round-trip", () => {
    const screen = blessedMock.screen();
    const log = blessedMock.log({ parent: screen, label: "Log" });
    log.setContent("hello");
    const logs = screen.findByKind("log");
    expect(logs).toHaveLength(1);
    expect(logs[0].getContent()).toBe("hello");
  });

  it("key() binds handler and emitKey fires it", () => {
    const screen = blessedMock.screen();
    let fired = 0;
    screen.key(["q"], () => { fired += 1; });
    expect(screen.hasKeyBinding("q")).toBe(true);
    expect(screen.emitKey("q")).toBe(true);
    expect(fired).toBe(1);
  });

  it("screen.typeKey delivers to every descendant with a binding", () => {
    const screen = blessedMock.screen();
    const a = blessedMock.box({ parent: screen });
    const b = blessedMock.box({ parent: screen });
    let aCount = 0;
    let bCount = 0;
    a.key(["Tab"], () => { aCount += 1; });
    b.key(["Tab"], () => { bCount += 1; });
    const r = screen.typeKey("Tab");
    expect(r.deliveredTo).toBe(2);
    expect(aCount).toBe(1);
    expect(bCount).toBe(1);
  });

  it("snapshot() captures kind/label/content/children tree", () => {
    const screen = blessedMock.screen();
    blessedMock.box({ parent: screen, label: "Header", content: "AzureClaw Operator" });
    blessedMock.log({ parent: screen, label: "Log", content: "ready" });
    const snap = screen.snapshot();
    expect(snap.kind).toBe("screen");
    const children = snap.children as Array<{ kind: string; label: string | null; content: string }>;
    expect(children).toHaveLength(2);
    expect(children[0].label).toBe("Header");
    expect(children[0].content).toBe("AzureClaw Operator");
    expect(children[1].kind).toBe("log");
  });

  it("destroy() detaches a node from its parent", () => {
    const screen = blessedMock.screen();
    const box = blessedMock.box({ parent: screen });
    expect(screen.children).toHaveLength(1);
    box.destroy();
    expect(box.isDestroyed()).toBe(true);
    expect(screen.children).toHaveLength(0);
  });

  it("blessedContribMock.grid.set places nodes on the screen", () => {
    const screen = blessedMock.screen();
    const grid = new blessedContribMock.grid({ rows: 12, cols: 12, screen });
    const table = grid.set(0, 0, 6, 12, blessedContribMock.table, { label: "Agents" });
    expect(table).toBeInstanceOf(MockNode);
    expect(table.parent).toBe(screen);
    expect(screen.findByKind("table")).toHaveLength(1);
  });
});

describe("compat: protected flow catalogue (plan §5.1)", () => {
  it("lists exactly the eight flows from the plan", () => {
    expect(PROTECTED_FLOWS).toHaveLength(8);
    const ids = PROTECTED_FLOWS.map((f: ProtectedFlow) => f.id).sort();
    expect(ids).toEqual(
      [
        "agt-interop",
        "azureclaw-connect",
        "azureclaw-dev",
        "azureclaw-handoff",
        "azureclaw-offload",
        "azureclaw-operator",
        "azureclaw-up",
        "plugin-lifecycle",
      ].sort(),
    );
  });

  it("every flow has a non-empty summary", () => {
    for (const f of PROTECTED_FLOWS) {
      expect(f.summary.length).toBeGreaterThan(10);
    }
  });
});

describe("compat: operator TUI — Phase 1 staging", () => {
  // These are intentionally todo in Phase 0. They land in Phase 1 alongside
  // the operator.ts decomposition (plan §7, `operator/{tui,input,data,overlays,keymap}.ts`).
  it.todo("renders header, agent table, security box, egress box, log box, status bar");
  it.todo("Tab cycles focus agents → egress → agents");
  it.todo("↑/↓ navigate rows within focused table");
  it.todo("'a' approves a learned egress domain via egress-manager mock");
  it.todo("'d' denies a learned egress domain via egress-manager mock");
  it.todo("'L' toggles learning ↔ enforcement and re-renders status bar");
  it.todo("'q' triggers graceful shutdown and exits with code 0");
  it.todo("kubectl-mock outage degrades header to 'cluster: unknown' without crashing");
});
