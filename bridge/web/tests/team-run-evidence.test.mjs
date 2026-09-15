// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const context = { exports: {}, URL };
runInNewContext(ts.transpileModule(readFileSync(
  new URL("../src/lib/team-run-evidence.ts", import.meta.url), "utf8",
), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText, context);

function analyze(artifacts, overrides = {}, names = ["qa"]) {
  return context.exports.analyzeTeamRun({
    paused: false,
    roster: names.map(name => ({ name, system_prompt: "" })),
  }, {
    artifacts,
    activity: [],
    result: null,
    launched: false,
    execution_phase: null,
    assignment: null,
    assignment_events: [],
    current_run_nonce: null,
    role_plan: { selected_roles: [], skipped_roles: [] },
    collaboration_events: [],
    ...overrides,
  });
}

const artifact = (source_agent, name = "qa-report.md") => ({
  name,
  source_agent,
  source_path: `/sandbox/${name}`,
  content: null,
  content_truncated: false,
});

test("explicit principal producer cannot become recorded QA through name or path", () => {
  const input = artifact("principal");
  const evidence = analyze([input]);
  assert.equal(evidence.roles[0].artifactAttribution, "none");
  assert.equal(evidence.roles[0].artifacts.length, 0);
  assert.equal(evidence.unattributedArtifacts[0], input);
  assert.equal(evidence.roles[0].state, "missing");
  assert.equal(evidence.outcome, "incomplete");
  const withPrincipal = analyze([input], {}, ["qa", "principal"]);
  assert.equal(withPrincipal.roles[0].artifacts.length, 0);
  assert.equal(withPrincipal.roles[1].artifactAttribution, "recorded");
});

test("matching explicit producer is recorded even if its filename names another role", () => {
  const evidence = analyze([artifact("qa", "developer-report.md")], {}, ["developer", "qa"]);
  assert.equal(evidence.roles[0].artifacts.length, 0);
  assert.equal(evidence.roles[1].artifactAttribution, "recorded");
});

test("filename and path without producer metadata are at most inferred", () => {
  for (const producer of [undefined, null, "", "   "]) {
    const evidence = analyze([artifact(producer)]);
    assert.equal(evidence.roles[0].artifactAttribution, "inferred");
    assert.equal(evidence.roles[0].state, "missing");
    assert.equal(evidence.outcome, "incomplete");
    assert.equal(evidence.collaboration[0].outcome, null);
    assert.match(evidence.collaboration[0].preview, /inferred/);
  }
  const unknown = analyze([artifact(null, "notes.md")]);
  assert.equal(unknown.roles[0].artifactAttribution, "none");
  assert.equal(unknown.unattributedArtifacts.length, 1);
});

test("unknown explicit producer stays unattributed; substrings are not role identities", () => {
  for (const producer of ["qa-supervisor", "principal-qa", "unknown"]) {
    assert.equal(analyze([artifact(producer)]).unattributedArtifacts.length, 1);
  }
});

test("punctuation-normalized role names are explicit matches, not filename proof", () => {
  const evidence = analyze(
    [artifact("Dependency_Security", "qa-report.md")], {}, ["qa", "dependency-security"],
  );
  assert.equal(evidence.roles[0].artifacts.length, 0);
  assert.equal(evidence.roles[1].artifactAttribution, "recorded");
});

test("mixed role artifacts retain per-file attribution for honest rendering", () => {
  const explicit = artifact("qa");
  const inferred = artifact(null, "qa-notes.md");
  const evidence = analyze([explicit, inferred]);
  assert.equal(evidence.roles[0].artifactAttribution, "inferred");
  assert.equal(context.exports.artifactRoleAttribution(evidence.roles[0].role, explicit), "recorded");
  assert.equal(context.exports.artifactRoleAttribution(evidence.roles[0].role, inferred), "inferred");
});

test("producer evidence does not supersede assignment nonce or partial persistence", () => {
  const input = artifact("qa");
  const stale = analyze([input], {
    launched: true,
    execution_phase: "Running",
    current_run_nonce: "new-run-assignment",
    assignment: { task_id: "old-run-assignment", state: "Completed" },
    result: { assignment_nonce: "old-run-assignment", status: "success" },
    assignment_events: [{
      task_id: "old-run-assignment", stage: "child_handback", child_role: "qa",
      state: "Completed", outcome: "success",
    }],
  });
  assert.equal(stale.outcome, "running");
  assert.equal(stale.roles[0].state, "missing");
  const partial = analyze([input], {
    current_run_nonce: "slot-specific-run-nonce",
    assignment: { task_id: "slot-specific-run-nonce", state: "Completed" },
    assignment_events: [{
      task_id: "slot-specific-run-nonce", stage: "child_handback", child_role: "qa",
      state: "Completed", outcome: "success",
    }],
    result: {
      assignment_nonce: "slot-specific-run-nonce", status: "success",
      artifact_persistence: "partial", artifact_count: 1, declared_artifact_count: 2,
    },
  });
  assert.equal(partial.outcome, "delivered_with_issues");
  assert.equal(partial.roles[0].state, "delivered");
});

test("role artifact renderer labels each file and explicit unassigned producers honestly", () => {
  const page = readFileSync(new URL(
    "../src/app/workspace/teams/[name]/runs/[run]/page.tsx", import.meta.url,
  ), "utf8");
  assert.match(page, /import \{ ArtifactSummary \} from "\.\/artifact-summary";/);
  assert.match(page, /<ArtifactSummary artifact=\{a\} role=\{r\.role\} \/>/);
  assert.match(page, /<ArtifactSummary artifact=\{a\} \/>/);

  const summary = readFileSync(new URL(
    "../src/app/workspace/teams/[name]/runs/[run]/artifact-summary.tsx", import.meta.url,
  ), "utf8");
  const renderer = {
    exports: {},
    require: (name) => {
      assert.equal(name, "@/lib/team-run-evidence");
      return context.exports;
    },
    element: (tag, props, ...children) => ({ tag, props, children }),
  };
  runInNewContext(ts.transpileModule(summary, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.React,
      jsxFactory: "element",
    },
  }).outputText, renderer);
  const cases = [
    [artifact("qa"), { name: "qa" }, "from qa · recorded producer"],
    [artifact(null), { name: "qa" }, "possibly qa · inferred attribution"],
    [artifact("Dependency_Security"), { name: "dependency-security" }, "from dependency security · recorded producer"],
    [artifact("principal"), undefined, "recorded producer: principal · not attributed to a roster role"],
    [artifact(null, "notes.md"), undefined, "producer unknown"],
    [artifact("   ", "notes.md"), undefined, "producer unknown"],
  ];
  for (const [input, role, label] of cases) {
    const tree = renderer.exports.ArtifactSummary({ artifact: input, role });
    assert.deepEqual(JSON.parse(JSON.stringify(tree)), {
      tag: "summary",
      props: { className: "cursor-pointer px-4 py-3" },
      children: [
        { tag: "span", props: { className: "font-medium" }, children: [input.name] },
        { tag: "span", props: { className: "ml-2 text-xs text-foreground-muted" }, children: [label] },
      ],
    });
  }
});
