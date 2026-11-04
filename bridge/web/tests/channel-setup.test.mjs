// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const ts = createRequire(import.meta.url)("typescript");
const jsx = (type, props) => ({ type, props: props ?? {} });
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree?.props) return [];
  return [tree, ...nodes(tree.props.children)];
}
function text(tree) {
  if (Array.isArray(tree)) return tree.map(text).join(" ");
  return tree?.props ? text(tree.props.children) : typeof tree === "string" ? tree : "";
}
function harness(scope, enabled = [], statuses = []) {
  const states = [], transitions = [], requests = [];
  let cursor = 0, effect, refreshes = 0;
  const props = scope === "team" ? { team: "monitor", enabled, statuses } : { ns: "work" };
  const imports = {
    react: {
      useState(initial) {
        const index = cursor++;
        if (!(index in states)) states[index] = initial;
        return [states[index], next => { states[index] = next; }];
      },
      useCallback: fn => fn,
      useEffect: fn => { effect ??= fn; },
      useTransition: () => [false, fn => { transitions.push(fn()); }],
    },
    "react/jsx-runtime": { jsx, jsxs: jsx },
    "@/components/icon": { Icon: "Icon" },
    "next/navigation": { useRouter: () => ({ refresh() { refreshes++; } }) },
    "./channel-actions": {
      async setTeamChannel(team, channel, token, allowFrom) {
        requests.push({ team, channel, token, allow_from: allowFrom });
        return {};
      },
      async deleteTeamChannel(team, channel) { requests.push({ team, channel, method: "DELETE" }); return {}; },
    },
  };
  const context = {
    exports: {},
    require(name) { assert.ok(name in imports, `Unexpected import ${name}`); return imports[name]; },
    window: { setTimeout(fn) { fn(); return 1; }, clearTimeout() {} },
    async fetch(url, init) {
      if (init.method === "POST") {
        const body = JSON.parse(init.body);
        requests.push({ url, ...body });
        enabled = [...enabled, body.channel];
      }
      return { ok: true, json: async () => ({ enabled }) };
    },
  };
  const file = scope === "team" ? "app/workspace/teams/[name]/team-channels.tsx" : "components/connect-channels.tsx";
  runInNewContext(ts.transpileModule(readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  const Component = scope === "team" ? context.exports.TeamChannels : context.exports.ConnectChannels;
  const render = () => { cursor = 0; return Component(props); };
  const button = label => nodes(render()).find(node => node.type === "button" && text(node) === label);
  const card = label => nodes(render()).find(node => node.type === "div" && node.props.className === "rounded-lg border border-border p-3"
    && nodes(node).some(child => child.type === "span" && text(child) === label));
  const connect = label => nodes(card(label)).find(node => node.type === "button" && text(node) === "Connect").props.onClick();
  const checkbox = () => nodes(render()).find(node => node.type === "input" && node.props.type === "checkbox");
  const input = label => nodes(render()).find(node => node.props["aria-label"] === label || node.props.placeholder === label);
  return { requests, render, button, card, connect, checkbox, input,
    async mount() { render(); effect?.(); await new Promise(setImmediate); },
    async save() { await button("Save").props.onClick(); await Promise.all(transitions); },
    get refreshes() { return refreshes; },
  };
}

for (const scope of ["workspace", "team"]) {
  test(`${scope}: WhatsApp checkbox submits the existing true flag only after selection`, async () => {
    const h = harness(scope); await h.mount(); h.connect("WhatsApp");
    assert.equal(h.checkbox().props.checked, false);
    assert.equal(h.button("Save").props.disabled, true);
    assert.equal(nodes(h.render()).filter(node => node.type === "input" && node.props.type === "password").length, 0);
    assert.ok(nodes(h.render()).some(node => node.type === "label" && text(node).includes("Enable WhatsApp")));
    assert.match(text(h.render()), /pairing is not completed or checked here/);
    await h.save(); assert.equal(h.requests.length, 0, "handler also rejects an unchecked flag");
    h.checkbox().props.onChange({ target: { checked: true } });
    assert.equal(h.button("Save").props.disabled, false);
    h.checkbox().props.onChange({ target: { checked: false } });
    assert.equal(h.button("Save").props.disabled, true);
    h.checkbox().props.onChange({ target: { checked: true } });
    await h.save();
    assert.equal(h.requests.length, 1);
    assert.equal(h.requests[0].channel, "whatsapp");
    assert.equal(h.requests[0].token, "true");
    assert.equal(h.requests[0].allow_from, undefined);
    assert.equal(h.checkbox(), undefined);
    if (scope === "team") assert.equal(h.refreshes, 1);
    else assert.match(text(h.card("WhatsApp")), /configured/);
  });

  test(`${scope}: cancel and channel switching clear the enable flag without saving`, async () => {
    const h = harness(scope); await h.mount(); h.connect("WhatsApp");
    h.checkbox().props.onChange({ target: { checked: true } });
    h.button("Cancel").props.onClick(); h.connect("WhatsApp");
    assert.equal(h.checkbox().props.checked, false);
    h.checkbox().props.onChange({ target: { checked: true } }); h.connect("Slack");
    assert.equal(h.input("Slack Bot OAuth token").props.value, "");
    assert.equal(h.button("Save").props.disabled, true);
    assert.equal(h.checkbox(), undefined);
    assert.equal(h.requests.length, 0);
  });

  test(`${scope}: actual credentials remain password inputs with unchanged submission`, async () => {
    for (const [channel, label] of [["Telegram", "Bot token"], ["Slack", "Bot OAuth token"], ["Discord", "Bot token"]]) {
      const h = harness(scope); await h.mount(); h.connect(channel);
      const field = h.input(`${channel} ${label}`);
      assert.equal(field.props.type, "password"); assert.equal(field.props.autoComplete, "off");
      assert.equal(h.checkbox(), undefined);
      field.props.onChange({ target: { value: "  test-only-secret  " } });
      if (channel === "Telegram") h.input("Allowed chat IDs (required for agents to send you updates)").props.onChange({ target: { value: " 123,456 " } });
      await h.save();
      assert.equal(h.requests.length, 1); assert.equal(h.requests[0].token, "test-only-secret");
      assert.equal(h.requests[0].channel, channel.toLowerCase());
      assert.equal(h.requests[0].allow_from, channel === "Telegram" ? "123,456" : undefined);
      assert.doesNotMatch(text(h.render()), /test-only-secret/);
    }
  });

  test(`${scope}: configured status does not claim successful connection or qualification`, async () => {
    const h = harness(scope, ["telegram"], [{ channel: "telegram", enabled: true, qualified: false, detail: "No retained delivery evidence" }]);
    await h.mount();
    const content = text(h.card("Telegram"));
    assert.match(content, /configured/); assert.doesNotMatch(content, /\bconnected\b/);
    assert.match(text(h.render()), /not that a message has been delivered/);
    assert.doesNotMatch(text(h.render()), /encrypted-at-rest/);
    if (scope === "team") {
      assert.match(content, /\bunqualified\b/); assert.doesNotMatch(content, /\bqualified\b/);
      assert.match(content, /No retained delivery evidence/);
    }
  });
}
