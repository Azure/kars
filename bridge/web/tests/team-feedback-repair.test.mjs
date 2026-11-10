// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const root = "../src/app/workspace/teams/[name]/";
function load(file, imports) {
  const exports = {};
  runInNewContext(ts.transpileModule(readFileSync(new URL(root + file, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, { exports, require: imports });
  return exports;
}
function elements(node) {
  if (Array.isArray(node)) return node.flatMap(elements);
  if (!node || typeof node !== "object" || !node.props) return [];
  return [node, ...elements(node.props.children)];
}
function harness(result = { error: null }) {
  const states = [], calls = [], transitions = [];
  let cursor = 0, refreshes = 0;
  const item = {
    id: "work-a", title: "Checklist", description: "Retain this work", depends_on: [],
    acceptance_criteria: ["Deliver checklist"], review_required: true, status: "pending",
    feedback: "Original verbose feedback",
    history: [{ run: "old-run", run_uid: "old-uid", assignment_nonce: "nonce", outcome: "changes_requested",
      finished_at: "2026-10-15T00:00:00Z", feedback: "Original rejection evidence" }],
  };
  const props = { team: "eng", teamUid: "team-uid", paused: true, tasks: [item] };
  const { TeamTasks } = load("team-tasks.tsx", name => {
    if (name === "react") return {
      useState(initial) {
        const index = cursor++;
        if (!(index in states)) states[index] = initial;
        return [states[index], value => { states[index] = typeof value === "function" ? value(states[index]) : value; }];
      },
      useTransition: () => [false, callback => transitions.push(callback())],
    };
    if (name === "next/navigation") return { useRouter: () => ({ refresh: () => refreshes++ }) };
    if (name === "@/components/navigation-link") return { default: "a" };
    if (name === "@/components/live-refresh") return { LiveRefresh: () => null };
    if (name === "@/lib/format") return { formatEvidenceTime: value => value };
    if (name === "./task-actions") return {
      amendTeamTaskFeedback: async (...args) => { calls.push(args); return result; },
      addTeamTask: () => assert.fail("unexpected create"),
      deleteTeamTask: () => assert.fail("unexpected delete"),
      reviewTeamTask: () => assert.fail("unexpected review decision"),
    };
    return require(name);
  });
  return {
    props, item, calls,
    render() { cursor = 0; return elements(TeamTasks(props)); },
    async settle() { await Promise.all(transitions); },
    get refreshes() { return refreshes; },
  };
}
const button = (h, label) => h.render().find(node => node.type === "button" && node.props.children === label);
const editor = h => h.render().find(node => node.type === "textarea" && node.props["aria-label"] === "Next-run feedback for Checklist");

test("feedback editor is offered only for paused pending work with a Team UID", () => {
  for (const status of ["pending", "active", "awaiting_review", "done", "retired"]) {
    for (const paused of [false, true]) {
      const h = harness();
      h.props.paused = paused;
      h.item.status = status;
      assert.equal(!!button(h, "Edit queued feedback"), paused && status === "pending");
    }
  }
  const h = harness();
  h.props.teamUid = "";
  assert.equal(button(h, "Edit queued feedback"), undefined);
});

test("saving uses the captured full snapshot and lossless feedback, never rebases on refresh", async () => {
  const h = harness();
  assert.ok(h.render().some(node => node.props["data-work-item-id"] === "work-a"));
  button(h, "Edit queued feedback").props.onClick();
  assert.equal(editor(h).props.value, h.item.feedback);
  const text = "  Revised 🦀 instructions\n\"quoted\"\\path\n";
  editor(h).props.onChange({ target: { value: text } });
  h.props.tasks = [{ ...h.item, feedback: "Concurrent revision" }];
  button(h, "Save feedback").props.onClick();
  await h.settle();
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0][0], "eng");
  assert.equal(h.calls[0][1], "team-uid");
  assert.equal(h.calls[0][2], h.item);
  assert.equal(h.calls[0][3], text);
  assert.equal(h.item.history[0].feedback, "Original rejection evidence");
  assert.equal(h.refreshes, 1);
  assert.equal(editor(h), undefined);
});

test("actionable failure keeps the exact draft and original evidence; Cancel makes no write", async () => {
  const message = "Objective actual: 4200, allowed: 4096. Shorten revision feedback.";
  const h = harness({ error: message });
  button(h, "Edit queued feedback").props.onClick();
  editor(h).props.onChange({ target: { value: "still oversized" } });
  button(h, "Save feedback").props.onClick();
  await h.settle();
  assert.equal(editor(h).props.value, "still oversized");
  assert.ok(h.render().some(node => node.props.role === "alert" && node.props.children === message));
  assert.equal(h.refreshes, 0);
  button(h, "Cancel").props.onClick();
  assert.equal(editor(h), undefined);
  assert.equal(h.calls.length, 1);
  assert.equal(h.item.feedback, "Original verbose feedback");
});

test("Resume, concurrent claim, deletion and Team recreation disable stale edits with usable Cancel", async () => {
  for (const mutation of ["resume", "claim", "delete", "recreate"]) {
    const h = harness();
    button(h, "Edit queued feedback").props.onClick();
    const staleSave = button(h, "Save feedback");
    if (mutation === "resume") h.props.paused = false;
    if (mutation === "claim") h.props.tasks = [{ ...h.item, status: "active" }];
    if (mutation === "delete") h.props.tasks = [];
    if (mutation === "recreate") h.props.teamUid = "other-team";
    assert.equal(button(h, "Save feedback"), undefined);
    assert.ok(h.render().some(node => node.props.role === "alert"));
    assert.ok(staleSave);
    button(h, "Cancel").props.onClick();
    assert.equal(h.calls.length, 0);
    assert.equal(h.render().some(node => node.props.role === "alert"), false);
  }
});

test("server action sends authenticated no-store PATCH with full identity and exact feedback", async () => {
  for (const status of [200, 400, 409]) {
    const calls = [], refreshes = [];
    const { amendTeamTaskFeedback, reviewTeamTask } = load("task-actions.ts", name => {
      if (name === "next/cache") return { revalidatePath: path => refreshes.push(path) };
      if (name === "@/lib/config") return { defaultNamespace: () => "space/name" };
      if (name === "@/lib/bff") return { authenticatedBffFetch: async (...args) => {
        calls.push(args);
        return new Response(JSON.stringify({ error: { message: "actual: 4200, allowed: 4096" } }), { status });
      } };
      return require(name);
    });
    const expected = harness().item;
    const feedback = "  Changed \"feedback\" 🦀\n";
    const result = await amendTeamTaskFeedback("team/name", "uid", expected, feedback);
    assert.equal(calls[0][0], "/api/namespaces/space%2Fname/teams/team%2Fname/tasks/work-a/feedback");
    assert.equal(calls[0][1].method, "PATCH");
    assert.equal(calls[0][1].cache, "no-store");
    assert.deepEqual(JSON.parse(calls[0][1].body), { team_uid: "uid", expected, feedback });
    assert.equal(result.error, status === 200 ? null : "actual: 4200, allowed: 4096");
    assert.equal(refreshes.length, status === 200 ? 1 : 0);
    await reviewTeamTask("eng", "work-a", "request_changes", { assignmentNonce: "nonce", runUid: "uid" }, feedback);
    assert.equal(JSON.parse(calls[1][1].body).feedback, feedback);
  }
});
