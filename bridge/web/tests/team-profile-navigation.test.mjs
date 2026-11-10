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
const { renderToStaticMarkup } = require("react-dom/server");
const source = readFileSync(new URL("../src/app/workspace/teams/page.tsx", import.meta.url), "utf8");
const profile = {
  name: "public-docs", namespace: "workspace", display_name: "Public documentation review",
  charter_template: "Review public documentation", phase: "Ready",
  roles: [{ name: "researcher" }, { name: "reviewer" }],
};

async function render({ profiles = [profile], teams = [{ name: "existing-team" }], profilesError, teamsError } = {}) {
  const calls = [];
  const exports = {};
  runInNewContext(ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, {
    exports,
    require(name) {
      if (name === "@/lib/bff") return {
        listProfiles: async () => { calls.push("profiles"); if (profilesError) throw profilesError; return profiles; },
        listTeams: async ns => { calls.push(["teams", ns]); if (teamsError) throw teamsError; return teams; },
      };
      if (name === "@/lib/config") return { defaultNamespace: () => "workspace" };
      if (name === "@/components/navigation-link") return { default: props => React.createElement("a", props) };
      if (name === "@/components/honest-state") return { HonestState: ({ title, detail }) => React.createElement("aside", null, title, detail) };
      if (name === "./teams-list") return { TeamsList: ({ teams }) => React.createElement("ul", null,
        teams.map(team => React.createElement("li", { key: team.name }, team.name))) };
      return require(name);
    },
  });
  const html = renderToStaticMarkup(await exports.default());
  assert.deepEqual(calls, [["teams", "workspace"], "profiles"]);
  return html;
}

test("Ready workspace profiles link to the existing editable draft, without creating or launching", async () => {
  const html = await render({ profiles: [profile, { ...profile, name: "foreign-profile", namespace: "another-workspace" }] });
  assert.match(html, /href="\/workspace\/teams\/new\?profile=public-docs"/);
  assert.match(html, /Use Public documentation review/);
  assert.match(html, /2 roles · Ready/);
  assert.match(html, /Nothing launches until you review and approve it/);
  assert.match(html, /existing-team/);
  assert.match(html, /href="\/workspace\/teams\/new"/);
  assert.doesNotMatch(html, /foreign-profile/);
});

test("unvalidated and unknown profiles cannot be selected", async () => {
  const html = await render({ profiles: [
    { ...profile, name: "pending", phase: "Pending" },
    { ...profile, name: "unknown", phase: null },
  ] });
  assert.doesNotMatch(html, /new\?profile=/);
  assert.match(html, /controller validation/);
  assert.match(html, /Status unavailable/);
  assert.match(html, /existing-team/);
});

test("profile inventory failure does not hide existing teams or charter creation", async () => {
  const html = await render({ profilesError: new Error("upstream failed") });
  assert.match(html, /Team profiles are unavailable/);
  assert.match(html, /existing-team/);
  assert.match(html, /href="\/workspace\/teams\/new"/);
  assert.doesNotMatch(html, /No team profiles are available/);
});

test("failed Team inventory does not hide available profile drafts", async () => {
  const html = await render({ teamsError: new Error("upstream failed") });
  assert.match(html, /Teams are unavailable/);
  assert.match(html, /new\?profile=public-docs/);
  assert.doesNotMatch(html, /No standing teams yet/);
});

test("empty workspace inventory stays honest without leaking foreign profiles", async () => {
  const html = await render({ teams: [], profiles: [{ ...profile, namespace: "another-workspace" }] });
  assert.match(html, /No team profiles are available in this workspace/);
  assert.match(html, /No standing teams yet/);
  assert.doesNotMatch(html, /new\?profile=/);
});

test("profile names are encoded and absent display names fall back to the resource name", async () => {
  const html = await render({ profiles: [{ ...profile, name: "profile&extra=value", display_name: null }] });
  assert.match(html, /href="\/workspace\/teams\/new\?profile=profile%26extra%3Dvalue"/);
  assert.match(html, /Use profile&amp;extra=value/);
});
