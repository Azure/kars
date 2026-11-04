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

function render(ledger, locale = "en-US", timeZone = "UTC") {
  class EnvironmentDate extends Date {
    toLocaleString(locales, options) {
      return super.toLocaleString(locales ?? locale, { timeZone, ...options });
    }
  }
  function load(path, imports) {
    const context = {
      exports: {}, Date: EnvironmentDate, locale,
      require(name) {
        assert.ok(Object.hasOwn(imports, name), `unexpected dependency: ${name}`);
        return imports[name];
      },
    };
    runInNewContext(`Number.prototype.toLocaleString = function(locales, options) {
      return new Intl.NumberFormat(locales ?? locale, options).format(this.valueOf());
    };\n${ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
    }).outputText}`, context);
    return context.exports;
  }
  const { TeamLedger } = load("../src/app/workspace/teams/[name]/team-ledger.tsx", {
    react: React,
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/components/navigation-link": {
      default: ({ children, ...props }) => React.createElement("a", props, children),
    },
    "@/components/deliverable-view": { toPlainPreview: (value) => value },
    "@/lib/format": load("../src/lib/format.ts", {}),
  });
  return renderToStaticMarkup(React.createElement(TeamLedger, { team: "clinic team", ledger }));
}

const event = { at: "2026-10-13T00:30:00Z", kind: "delivery", summary: "Retained result", task: "run/one", tokens: 1234567 };

test("ledger timestamps and token counts match across server and browser locales/timezones", () => {
  const server = render([event]);
  for (const [locale, zone] of [["en-GB", "Europe/London"], ["de-DE", "America/Los_Angeles"], ["ja-JP", "Asia/Tokyo"]]) {
    assert.equal(render([event], locale, zone), server, `${locale}/${zone}`);
  }
  assert.match(server, /2026-10-13 00:30:00 UTC/);
  assert.match(server, /<time[^>]+dateTime="2026-10-13T00:30:00Z"/);
  assert.match(server, /title="2026-10-13T00:30:00Z"/);
  assert.match(server, /1,234,567t/);
  assert.match(server, /\/workspace\/teams\/clinic%20team\/runs\/run%2Fone/);
});

test("missing, malformed and timezone-less ledger times remain explicitly unavailable", () => {
  for (const at of ["", "not-a-time", "2026-10-13T00:30:00"]) {
    const html = render([{ ...event, at }]);
    assert.match(html, /Time unavailable/);
    assert.doesNotMatch(html, /Invalid Date/);
  }
});

test("ledger retains its bounded initial page and expansion control", () => {
  const html = render(Array.from({ length: 35 }, (_, i) => ({ ...event, summary: `Event ${i}` })));
  assert.equal((html.match(/<li /g) ?? []).length, 30);
  assert.match(html, /Show all 35 events/);
  assert.doesNotMatch(html, /Event 30</);
});

test("an empty ledger does not invent activity or timestamps", () => {
  const html = render([]);
  assert.match(html, /No activity recorded yet/);
  assert.doesNotMatch(html, /<time|<button/);
});
