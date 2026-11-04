// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const React = require("react");
const { jsx } = require("react/jsx-runtime");
const { renderToStaticMarkup } = require("react-dom/server");

function load(path, imports, globals = {}) {
  const context = { exports: {}, ...globals, require(name) {
    assert.ok(Object.hasOwn(imports, name), `unexpected dependency: ${name}`);
    return imports[name];
  } };
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}

function find(node, predicate) {
  if (!React.isValidElement(node)) return null;
  if (predicate(node)) return node;
  for (const child of React.Children.toArray(node.props.children)) {
    const result = find(child, predicate);
    if (result) return result;
  }
  return null;
}

function formFixture({ override, failed = false } = {}) {
  const state = [], requests = [];
  let cursor = 0, refreshed = 0;
  let sandboxes = [{ name: "reviewer", runtime: "Hermes" }];
  const { NewEvalForm } = load("../src/app/console/evals/new-eval-form.tsx", {
    "react/jsx-runtime": require("react/jsx-runtime"),
    react: { useState(initial) {
      const index = cursor++;
      if (!(index in state)) state[index] = initial;
      return [state[index], value => { state[index] = value; }];
    } },
    "next/navigation": { useRouter: () => ({ refresh: () => refreshed++ }) },
  }, { fetch: async (url, options) => {
    requests.push({ url, body: JSON.parse(options.body) });
    return { ok: !failed, status: failed ? 503 : 200, json: async () => ({ error: { message: "Runner unavailable" } }) };
  } });
  const render = () => { cursor = 0; return NewEvalForm({
    sandboxes,
    ...(override ? { defaultRunnerImage: override } : {}),
  }); };
  render().props.onClick();
  return { render, state, requests, refreshCount: () => refreshed,
    setSandboxes(value) { sandboxes = value; } };
}

test("new evals inherit the configured Core runner rather than forcing a development image", async () => {
  const f = formFixture();
  assert.match(renderToStaticMarkup(f.render()), /Core-configured runner/);
  await f.render().props.onSubmit({ preventDefault() {} });
  assert.equal(f.requests[0].body.runner_image, null);
  assert.equal(f.requests[0].body.target_sandbox, "reviewer");
  assert.equal(f.requests[0].body.run_now, true);
  assert.equal(f.refreshCount(), 1);
});

test("a target removed by live refresh cannot be submitted until an available sandbox is selected", async () => {
  const f = formFixture();
  f.setSandboxes([{ name: "standing-sre", runtime: "Hermes" }]);
  let tree = f.render();
  assert.equal(find(tree, n => n.type === "select").props.value, "");
  assert.equal(find(tree, n => n.type === "button" && n.props.type === "submit").props.disabled, true);
  await tree.props.onSubmit({ preventDefault() {} });
  assert.equal(f.requests.length, 0);
  assert.match(renderToStaticMarkup(f.render()), /no longer available for a new eval/);
  tree = f.render();
  find(tree, n => n.type === "select").props.onChange({ target: { value: "standing-sre" } });
  await f.render().props.onSubmit({ preventDefault() {} });
  assert.equal(f.requests.length, 1);
  assert.equal(f.requests[0].body.target_sandbox, "standing-sre");
  assert.equal(f.refreshCount(), 1);
});

test("an explicit runner image is retained, including after a failed request", async () => {
  const f = formFixture({ override: "mirror.example/runner:latest", failed: true });
  await f.render().props.onSubmit({ preventDefault() {} });
  assert.equal(f.requests[0].body.runner_image, "mirror.example/runner:latest");
  assert.equal(f.refreshCount(), 0);
  const html = renderToStaticMarkup(f.render());
  assert.match(html, /Runner unavailable/);
  assert.match(html, /role="alert"/);
  assert.match(html, /mirror\.example\/runner:latest/);
});

test("every offered builtin corpus is actually shipped and all shipped corpora are offered", () => {
  const source = readFileSync(new URL("../../../eval-corpus/src/builtin_names.rs", import.meta.url), "utf8");
  const declaration = source.match(/pub const BUILTIN_NAMES:.*?=\s*&\[([\s\S]*?)\];/);
  assert.ok(declaration, "controller corpus catalogue declaration");
  const expected = [...declaration[1].matchAll(/"([^"]+)"/g)].map(match => match[1]);
  const options = find(formFixture().render(), n => n.type === "datalist");
  assert.deepEqual(React.Children.toArray(options.props.children).map(option => option.props.value), expected);
});

test("the form supports native keyboard submission and freezes edits while launching", async () => {
  const f = formFixture();
  assert.equal(f.render().type, "form");
  assert.equal(find(f.render(), n => n.type === "button" && n.props.children === "Configure & run").props.type, "submit");
  f.state[5] = true;
  const tree = f.render();
  assert.equal(tree.props["aria-busy"], true);
  assert.equal(find(tree, n => n.type === "fieldset").props.disabled, true);
  await tree.props.onSubmit({ preventDefault() {} });
  assert.equal(f.requests.length, 0);
});

async function renderPage({ sandboxFails = false, evalFails = false, result = null,
  sandboxes = [{ name: "reviewer", phase: "Running", runtime: "Hermes" }], onTargets = () => {} } = {}) {
  const imports = {
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/components/ui": {
      PageHeader: ({ title, lead }) => jsx("header", { children: [
        jsx("h1", { children: title }, "title"),
        jsx("p", { children: lead }, "lead"),
      ] }),
      Stat: ({ label, value }) => jsx("div", { children: `${label}: ${value}` }),
      Badge: ({ children }) => children,
    },
    "@/components/honest-state": { HonestState: ({ title, detail }) => jsx("p", { children: `${title}: ${detail}` }) },
    "@/components/live-refresh": { LiveRefresh: ({ active, intervalMs }) => {
      assert.equal(active, true);
      assert.equal(intervalMs, 5000);
      return null;
    } },
    "@/lib/bff": {
      listEvals: async () => {
        if (evalFails) throw new Error("private evaluation detail");
        return result ? [{ name: "retained-eval", namespace: "kars-system", phase: "Ready", last_result: result }] : [];
      },
      listSandboxes: async () => {
        if (sandboxFails) throw new Error("private sandbox detail");
        return sandboxes;
      },
    },
    "./new-eval-form": { NewEvalForm: ({ sandboxes }) => {
      onTargets(sandboxes);
      return jsx("button", { children: "Launch eval fixture" });
    } },
    "./eval-detail": { EvalDetail: () => null },
  };
  return renderToStaticMarkup(await load("../src/app/console/evals/page.tsx", imports).default());
}

test("sandbox inventory failure is visible while retained evaluation reports remain available", async () => {
  const html = await renderPage({ sandboxFails: true, result: { total: 1, passed: 1, failed: 0, errored: 0 } });
  assert.match(html, /Sandbox inventory unavailable/);
  assert.match(html, /retained-eval/);
  assert.doesNotMatch(html, /Launch eval fixture|private sandbox detail/);
});

test("evaluation read failure is not presented as an empty or healthy feature", async () => {
  const html = await renderPage({ evalFails: true });
  assert.match(html, /Evaluation history unavailable/);
  assert.doesNotMatch(html, /No safety evals yet|Launch eval fixture|private evaluation detail/);
});

test("a successful empty read remains an empty history with a working creation entry point", async () => {
  const html = await renderPage();
  assert.match(html, /No safety evals yet/);
  assert.match(html, /Launch eval fixture/);
  assert.doesNotMatch(html, /unavailable/);
});

test("eval choices exclude delivered missions but retain active missions and standing sandboxes", async () => {
  let targets;
  await renderPage({
    sandboxes: [
      { name: "delivered", phase: "Running", executing: false, runtime: "Hermes" },
      { name: "delivered-ready", phase: "Ready", executing: false, runtime: "OpenClaw" },
      { name: "active", phase: "Running", executing: true, runtime: "Hermes" },
      { name: "sre", phase: "Running", executing: null, runtime: "Hermes" },
      { name: "orchestrator", phase: "Ready", runtime: "OpenClaw" },
      { name: "pending", phase: "Pending", executing: true, runtime: "Hermes" },
    ],
    onTargets(value) { targets = value; },
  });
  assert.deepEqual(Array.from(targets, target => target.name), ["active", "sre", "orchestrator"]);
});

test("zero-case reports never claim that every attack was handled", async () => {
  const html = await renderPage({ result: { total: 0, passed: 0, failed: 0, errored: 0 } });
  assert.match(html, /No cases were evaluated/);
  assert.doesNotMatch(html, /Every evaluated case matched its declared expectations/);
});

test("errored runs are inconclusive without inventing a target-unreachable diagnosis", async () => {
  const html = await renderPage({ result: { total: 2, passed: 1, failed: 0, errored: 1 } });
  assert.match(html, /inconclusive/);
  assert.doesNotMatch(html, /Every evaluated case matched its declared expectations|target unreachable|No safety failures detected/);
});

test("complete passing and failing reports retain their actual verdicts", async () => {
  assert.match(await renderPage({ result: { total: 2, passed: 2, failed: 0, errored: 0 } }),
    /Every evaluated case matched its declared expectations/);
  assert.match(await renderPage({ result: { total: 2, passed: 1, failed: 1, errored: 0 } }),
    /1 case did not match its declared expectations/);
});

test("a failed summary does not invent an attack bypass from a baseline mismatch", async () => {
  const html = await renderPage({ result: { total: 2, passed: 1, failed: 1, errored: 0 } });
  assert.match(html, /1 case did not match its declared expectations/);
  assert.match(html, /decision, policy attribution, or reason/);
  assert.doesNotMatch(html, /attack got through|attack was NOT blocked/);
});

test("a blocked case with the wrong policy remains a failure without claiming it was allowed", () => {
  const state = [true, { name: "retained-eval", version: null, report: {
    corpus: "egress-known-bad", completed_at: "2026-10-10T00:00:00Z", per_case_available: true,
    total: 1, passed: 0, failed: 1, errored: 0,
    cases: [{ id: "blocked-by-wrong-policy", expected: "Blocked", actual: "Blocked",
      actual_reason: "ByPolicyKindMismatch", pass: false, errored: false, tags: [] }],
  } }, false, null];
  let index = 0;
  const { EvalDetail } = load("../src/app/console/evals/eval-detail.tsx", {
    "react/jsx-runtime": require("react/jsx-runtime"),
    react: { useState: () => [state[index++], () => {}], useEffect() {} },
  });
  const html = renderToStaticMarkup(jsx(EvalDetail, { name: "retained-eval" }));
  assert.match(html, />FAIL</);
  assert.match(html, /expected.*Blocked.*actual.*Blocked/);
  assert.match(html, /ByPolicyKindMismatch/);
  assert.doesNotMatch(html, /adversarial|attack.*blocked/);
});

test("strict egress corpus prerequisites are explicit without changing the selected policy", () => {
  const f = formFixture();
  const corpus = find(f.render(), n => n.type === "input" && n.props.list === "corpus-options");
  corpus.props.onChange({ target: { value: "egress-known-bad" } });
  const html = renderToStaticMarkup(f.render());
  assert.match(html, /strict egress enforcement/);
  assert.match(html, /Learn mode or approved destinations/);
  assert.match(html, /Transport errors are not passing policy verdicts/);
  assert.match(html, /Task-bound governed inference rejects opaque tunnels/);
  assert.match(html, /unbounded allowance; use governed-egress-tunnels/);
  assert.match(html, /Calendar-only accounting does not enable this tunnel fence/);
  assert.equal(f.requests.length, 0);
});

test("governed egress guidance preserves the separate corpus and never implies allowlist acceptance", async () => {
  const f = formFixture();
  const corpus = find(f.render(), n => n.type === "input" && n.props.list === "corpus-options");
  corpus.props.onChange({ target: { value: "governed-egress-tunnels" } });
  const html = renderToStaticMarkup(f.render());
  assert.match(html, /Requires task-bound governed inference, including unbounded allowances/);
  assert.match(html, /Calendar-only accounting is not sufficient/);
  assert.match(html, /proxy still evaluates destination policies/);
  assert.match(html, /explicit opaque-tunnel refusal/);
  assert.match(html, /does not test destination-allowlist/);
  assert.match(html, /generic denials and transport errors do not pass/);
  await f.render().props.onSubmit({ preventDefault() {} });
  assert.equal(f.requests[0].body.corpus, "governed-egress-tunnels");
  assert.equal(f.requests[0].body.runner_image, null);
});

test("open reports refresh on a new run, hiding stale downloads and cancelling old reads", async () => {
  const state = [true, null, false, null], pending = [];
  let cursor = 0, deps, cleanup;
  const { EvalDetail } = load("../src/app/console/evals/eval-detail.tsx", {
    "react/jsx-runtime": require("react/jsx-runtime"),
    react: {
      useState() {
        const index = cursor++;
        return [state[index], value => { state[index] = value; }];
      },
      useEffect(effect, nextDeps) {
        if (deps && deps.every((value, index) => value === nextDeps[index])) return;
        cleanup?.();
        deps = nextDeps;
        cleanup = effect();
      },
    },
  }, {
    AbortController,
    fetch: (url, options) => new Promise(resolve => pending.push({ url, options, resolve })),
  });
  const render = version => {
    cursor = 0;
    return EvalDetail({ name: "retained-eval", version });
  };
  const settle = () => new Promise(resolve => setImmediate(resolve));
  const body = { cases: [], completed_at: null, corpus: "egress-known-bad" };
  render("run-1");
  assert.equal(pending.length, 1);
  render("run-1");
  assert.equal(pending.length, 1);
  render("run-2");
  assert.equal(pending[0].options.signal.aborted, true);
  pending[0].resolve({ ok: true, json: async () => body });
  await settle();
  assert.equal(state[1], null);
  pending[1].resolve({ ok: true, json: async () => body });
  await settle();
  assert.equal(state[1].version, "run-2");
  assert.match(renderToStaticMarkup(render("run-2")), /Download report/);
  assert.doesNotMatch(renderToStaticMarkup(render("run-3")), /Download report/);
  assert.equal(state[0], true);
  pending[2].resolve({ ok: false, status: 503 });
  await settle();
  assert.match(renderToStaticMarkup(render("run-3")), /load failed \(503\)/);
  cleanup();
});
