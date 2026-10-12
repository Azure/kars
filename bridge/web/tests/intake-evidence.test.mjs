// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";
import { classifyIntent } from "../src/lib/classify-intent.ts";
import { patternsFor, scaffoldObjective } from "../src/lib/loop-patterns.ts";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");

function load(path, imports = {}) {
  const exports = {};
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require: name => imports[name] ?? require(name) });
  return exports;
}

function descendants(node) {
  if (!node || typeof node !== "object") return [];
  if (Array.isArray(node)) return node.flatMap(descendants);
  return [node, ...descendants(node.props?.children)];
}

function intake(intent, override = null, pending = false) {
  const states = [intent, override];
  const routes = [];
  let cursor = 0;
  const { IntentEntry } = load("../src/components/intent-entry.tsx", {
    react: { ...React, useMemo: fn => fn(), useState: () => {
      const index = cursor++;
      return [states[index], value => { states[index] = value; }];
    }, useTransition: () => [pending, fn => fn()] },
    "next/navigation": { useRouter: () => ({ push: href => routes.push(href) }) },
    "@/lib/classify-intent": { classifyIntent },
    "@/components/icon": { Icon: () => null },
  });
  return { routes, render() { cursor = 0; return IntentEntry(); } };
}

test("intake routes bounded collaboration to Mission without claiming model confidence", () => {
  const intent = "Create a one-off research package using multiple agents, not a recurring team.";
  const f = intake(intent);
  const tree = f.render();
  const html = renderToStaticMarkup(tree);
  assert.match(html, /Suggested: .*a one-off mission/);
  assert.match(html, /Based on wording/);
  assert.doesNotMatch(html, /high confidence/i);
  descendants(tree).find(n => n.type === "button" && n.props.children === "Compose the mission →").props.onClick();
  assert.deepEqual(f.routes, [`/workspace/new?intent=${encodeURIComponent(intent)}`]);
});

test("explicit selection is labelled Selected and retained on navigation", () => {
  const f = intake("Monitor alerts daily.");
  let tree = f.render();
  descendants(tree).find(n => n.type === "button" && n.props.children === "Mission").props.onClick();
  tree = f.render();
  const html = renderToStaticMarkup(tree);
  assert.match(html, /Selected: .*a one-off mission/);
  assert.doesNotMatch(html, /Suggested:|Based on wording/);
  assert.equal(descendants(tree).find(n => n.props.children === "Mission").props["aria-pressed"], true);
  descendants(tree).find(n => n.type === "button" && n.props.children === "Compose the mission →").props.onClick();
  assert.match(f.routes[0], /^\/workspace\/new\?/);
});

test("generic noncoding standing intent reaches the Team composer with its full charter", () => {
  const intent = "Maintain a standing, noncoding operations-review team. Review capacity, find risks, and produce documents; retain decisions across later requests.";
  const f = intake(intent);
  const tree = f.render();
  assert.match(renderToStaticMarkup(tree), /Suggested: .*a standing team/);
  descendants(tree).find(n => n.type === "button" && n.props.children === "Compose the team →").props.onClick();
  assert.deepEqual(f.routes, [`/workspace/teams/new?intent=${encodeURIComponent(intent)}`]);
});

test("pending composition prevents edits, mode changes, example clicks and keyboard resubmission", () => {
  const f = intake("Create a one-off report.", null, true);
  const tree = f.render();
  const controls = descendants(tree).filter(n => n.type === "button" || n.type === "textarea");
  assert.ok(controls.length >= 7);
  assert.ok(controls.every(n => n.props.disabled));
  const input = controls.find(n => n.type === "textarea");
  input.props.onKeyDown({ metaKey: true, key: "Enter" });
  assert.deepEqual(f.routes, []);
});

test("sandbox ancestry is not labelled as verified AgentMesh communication", () => {
  const { MeshTopology } = load("../src/app/console/fleet/mesh-topology.tsx");
  const html = renderToStaticMarkup(React.createElement(MeshTopology, { sandboxes: [
    { name: "principal", runtime: "openclaw", phase: "Running", parent: null },
    { name: "worker", runtime: "hermes", phase: "Running", parent: "principal" },
  ] }));
  assert.match(html, /1 delegation relationship/);
  assert.match(html, /do not establish.*exchanged messages/);
  assert.doesNotMatch(html, /proven encrypted|encrypted links/);
  assert.match(html, /delivery, decryption and replies are not verified/);
});

function loopDesigner(props) {
  const { LoopDesigner } = load("../src/components/loop-designer.tsx", {
    "@/lib/loop-patterns": { patternsFor, scaffoldObjective },
    "@/components/icon": { Icon: () => null },
  });
  return renderToStaticMarkup(React.createElement(LoopDesigner, {
    surface: "mission", defaultOpen: true, initialGoal: "Review clinic operations", ...props,
  }));
}

test("unsupported proposed loops are explicitly explained without selecting a substitute", () => {
  const html = loopDesigner({ initialPatternId: "standing-watch" });
  assert.match(html, /role="alert"/);
  assert.match(html, /not available for this mission/);
  assert.match(html, /Choose a compatible pattern/);
  assert.doesNotMatch(html, /Generated objective|Use this loop/);
  assert.match(html, /Reflect-Refine/);
});

test("heuristic loop suggestions never claim model authorship", () => {
  const html = loopDesigner({
    initialPatternId: "reflect", proposalSource: "heuristic",
    rationale: "Suggested from keywords because the orchestrator returned an unsupported loop.",
  });
  assert.match(html, /Suggested starting point:/);
  assert.doesNotMatch(html, /Orchestrator&#x27;s initial proposal:/);
  assert.match(html, /Generated objective/);
});

test("compatible model loop proposals retain their attribution and usable review controls", () => {
  const html = loopDesigner({
    initialPatternId: "reflect", proposalSource: "orchestrator", rationale: "Critique before approval.",
  });
  assert.match(html, /Orchestrator&#x27;s initial proposal:/);
  assert.match(html, /Use this loop/);
  assert.doesNotMatch(html, /role="alert"/);
});
