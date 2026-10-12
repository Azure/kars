// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import test from "node:test";
import { classifyIntent } from "../src/lib/classify-intent.ts";

test("explicitly negated teams do not outweigh a one-off Mission", () => {
  for (const intent of [
    "Create one small one-off Mission, not a standing Team, to verify MCP.",
    "Create a one-off local Skill verification mission, not a standing Team.",
    "Write a report without a team.",
    "One-off mission, no persistent team.",
  ]) {
    assert.equal(classifyIntent(intent).kind, "mission", intent);
  }
});

test("affirmative standing and recurring work still recommends a team", () => {
  for (const intent of [
    "Create a standing team to monitor support every day.",
    "Keep an eye on the repository and review it daily.",
    "Not a standing team for research, but monitor alerts continuously every hour.",
  ]) {
    assert.equal(classifyIntent(intent).kind, "team", intent);
  }
});

test("empty and ordinary discrete requests retain their recommendations", () => {
  assert.equal(classifyIntent("").confidence, "low");
  assert.equal(classifyIntent("Research and write a report").kind, "mission");
  assert.equal(classifyIntent("team").kind, "team");
});

test("the exact complex browser-research request remains a one-off mission", () => {
  const intent = "Create a one-off engineering decision package comparing Playwright, Puppeteer, and Selenium for repeatable browser acceptance of an agent platform. Use the installed Playwright MCP service to visit official public documentation for all three, inspect at least six relevant pages, and record their actual titles, URLs and retrieved evidence. Compare browser coverage, session isolation, accessibility inspection, downloads, network diagnostics, and failure recovery. Produce three downloadable files: browser-acceptance-decision.md with a justified recommendation and explicit uncertainties, browser-capabilities.csv with cited evidence per criterion, and acceptance-checklist.md with at least twelve concrete positive and negative test cases and expected outcomes. Cross-check the CSV against the recommendation and cite actual visited pages, not remembered claims. Use local files for the deliverables; do not create issues, pull requests, accounts or external changes. If an installed tool fails or is denied, report the exact failure and incomplete work honestly rather than inventing evidence. Keep this a bounded one-off mission, not a recurring team.";
  assert.equal(classifyIntent(intent).kind, "mission");
  assert.match(classifyIntent(intent).reason, /multiple agents/);
});

test("collaboration and the subject of a report do not require standing work", () => {
  for (const intent of [
    "Use multiple agents and multiple roles to research and write a decision package.",
    "Use a team of researchers to write a report.",
    "Review the finance team org chart.",
    "Create a one-off report about daily monitoring, recurring incidents and standing teams.",
    "Create a one\u2011off mission, not a recurring team.",
    "Perform a single-run audit of our monitoring team.",
    "One-time task: inspect the weekly reports.",
  ]) assert.equal(classifyIntent(intent).kind, "mission", intent);
});

test("negating a bounded assignment does not override affirmative recurring work", () => {
  for (const intent of [
    "Not a one-off task: monitor alerts continuously every hour.",
    "Not a one-time mission. Keep an eye on the repository daily.",
    "No single-run report, monitor availability weekly.",
  ]) assert.equal(classifyIntent(intent).kind, "team", intent);
});

test("an explicit standing charter is not outweighed by its deliverable verbs", () => {
  for (const intent of [
    "Maintain a standing, noncoding operations-review team for a community repair clinic. Review capacity, find risks, and produce two documents. Retain shared decisions across later requests.",
    "Set up a persistent operations team to research constraints, find changes, and draft reports for human approval.",
    "We need an ongoing community review team to investigate issues and generate recommendations.",
  ]) {
    assert.equal(classifyIntent(intent).kind, "team", intent);
    assert.equal(classifyIntent(intent).confidence, "high", intent);
  }
});

test("a standing team mentioned as a subject or a rejected action stays a Mission", () => {
  for (const intent of [
    "Write a report about a standing operations team.",
    "Create a report about a standing team and review its mandate.",
    "Do not create a standing team. Research and write a report.",
    "Create a one-off report describing how to maintain a standing operations team.",
  ]) assert.equal(classifyIntent(intent).kind, "mission", intent);
});
