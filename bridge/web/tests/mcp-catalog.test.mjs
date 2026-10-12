// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";
import { MCP_CATALOG, MCP_CATEGORIES } from "../src/app/console/mcp-catalog-data.ts";
import { mcpStatus } from "../src/app/console/mcp-status.ts";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { renderToStaticMarkup } = require("react-dom/server");
const source = readFileSync(new URL("../src/app/console/mcp-catalog.tsx", import.meta.url), "utf8");
const entry = MCP_CATALOG.find(e => e.id === "playwright");
assert.ok(entry);

function load(states = [], save = async () => assert.fail("No mutation expected"), pending = false) {
  const exports = {};
  let cursor = 0;
  let result = { error: null, ok: null };
  const actions = [];
  runInNewContext(ts.transpileModule(source + "\nexport { AddForm };", {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, {
    exports,
    require(name) {
      if (name === "react") return { ...React,
        useState(initial) {
          const i = cursor++;
          if (!(i in states)) states[i] = typeof initial === "function" ? initial() : initial;
          return [states[i], value => { states[i] = typeof value === "function" ? value(states[i]) : value; }];
        },
        useMemo: fn => fn(),
        useActionState(fn) {
          const action = async (...args) => { result = await fn(...args); return result; };
          actions.push(action);
          return [result, action, pending];
        },
      };
      if (name === "./governance-actions") return { applyGovernanceAction: save };
      if (name === "./mcp-catalog-data") return { MCP_CATALOG, MCP_CATEGORIES };
      if (name === "./mcp-status") return { mcpStatus };
      if (name === "@/components/icon") return { Icon: () => null };
      return require(name);
    },
  });
  return { actions, render(name, props) { cursor = 0; return exports[name](props); } };
}

function descendants(node) {
  if (!node || typeof node !== "object") return [];
  if (Array.isArray(node)) return node.flatMap(descendants);
  return [node, ...descendants(node.props?.children)];
}

test("cancelling a catalog form does not mark anything installed or submit a request", () => {
  const states = [true, "All", "", "playwright", false];
  const f = load(states);
  let tree = f.render("McpCatalog", { servers: [] });
  const form = descendants(tree).find(n => n.props?.entry?.id === "playwright");
  assert.ok(form);
  form.props.onDone();
  assert.equal(states[3], null);
  tree = f.render("McpCatalog", { servers: [] });
  const html = renderToStaticMarkup(tree);
  assert.doesNotMatch(html, /added|View service|installation requested/);
  assert.equal(f.actions.length, 0);
});

test("catalog status and navigation come from the actual server list, not local click history", () => {
  const f = load([true, "All", "", null, false]);
  const servers = [{ name: "playwright", namespace: "kars-system", phase: "Degraded",
    status_current: true, status_message: "Probe failed" }];
  let html = renderToStaticMarkup(f.render("McpCatalog", { servers }));
  assert.match(html, /href="#mcp-kars-system-playwright"/);
  assert.match(html, /View service · Degraded/);
  servers[0].phase = "Ready";
  servers[0].status_current = false;
  html = renderToStaticMarkup(f.render("McpCatalog", { servers }));
  assert.match(html, /View service · Reconciling/);
  assert.doesNotMatch(html, /View service · Ready/);
});

test("save reports the request outcome and recovers controls without losing a failed draft", async () => {
  for (const outcome of ["saved", "denied", "transport"]) {
    const states = ["custom-playwright", "", "browser_navigate", false, ""];
    const busy = [];
    const f = load(states, async () => {
      if (outcome === "transport") throw new Error("PRIVATE");
      return outcome === "saved" ? { ok: "Applied", error: null } : { ok: null, error: "Denied" };
    });
    const props = { entry, onDone() { assert.fail("Submitting must not dismiss the outcome"); },
      onBusy: value => busy.push(value) };
    f.render("AddForm", props);
    const result = await f.actions[0]({ ok: null, error: null }, new FormData());
    assert.deepEqual(busy, [true, false]);
    const html = renderToStaticMarkup(f.render("AddForm", props));
    if (outcome === "saved") {
      assert.match(html, /role="status"/);
      assert.match(html, /installation requested/);
      assert.match(html, /not a readiness result/);
    } else {
      assert.equal(states[0], "custom-playwright");
      assert.equal(states[2], "browser_navigate");
      assert.match(html, /role="alert"/);
      if (outcome === "transport") {
        assert.match(result.error, /Couldn't confirm/);
        assert.doesNotMatch(html, /PRIVATE/);
      }
    }
  }
});

test("in-flight requests disable form edits, cancellation and catalog navigation", () => {
  const f = load([], undefined, true);
  const html = renderToStaticMarkup(f.render("AddForm", { entry, onDone() {}, onBusy() {} }));
  assert.match(html, /aria-busy="true"/);
  assert.match(html, /<fieldset disabled=""/);
  assert.match(html, /<button type="button" disabled=""[^>]*>Cancel/);
  const catalog = load([true, "All", "", null, true]);
  const tree = catalog.render("McpCatalog", { servers: [] });
  assert.ok(descendants(tree).filter(n => n.type === "button").every(n => n.props.disabled));
});
