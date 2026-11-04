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
class BffError extends Error {
  constructor(code, status) {
    super(code);
    this.code = code;
    this.status = status;
  }
}

function load(path, imports, globals = {}) {
  const context = {
    ...globals,
    exports: {},
    require(name) {
      assert.ok(Object.hasOwn(imports, name), `unexpected dependency: ${name}`);
      return imports[name];
    },
  };
  runInNewContext(ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX },
  }).outputText, context);
  return context.exports;
}

const empty = () => null;
const ui = {
  PageHeader: ({ title }) => React.createElement("h1", null, title),
  Section: ({ children }) => children,
  Badge: ({ children }) => children,
  Stat: ({ label, value }) => React.createElement("p", null, `${label}: ${value}`),
};
const imports = {
  "react/jsx-runtime": require("react/jsx-runtime"),
  "@/components/ui": ui,
  "@/components/icon": { Icon: empty },
  "@/app/console/audit/audit-search": { AuditSearch: empty },
  "@/lib/bff": { BffError },
};
const { AuditView } = load("../src/components/audit-view.tsx", imports);

for (const path of ["../src/app/audit/page.tsx", "../src/app/console/audit/page.tsx"]) {
  for (const [error, title, detail] of [
    [new BffError("forbidden", 403), "Audit access denied", "auditor or admin role"],
    [new BffError("unauthorized", 401), "Sign in to view audit evidence", "session is missing or expired"],
    [new BffError("upstream", 502), "Audit evidence unavailable", "does not mean the log is empty"],
    [new Error("connection failure"), "Audit evidence unavailable", "does not mean the log is empty"],
  ]) {
    test(`${path} preserves ${error.message} rather than reporting a false substrate outage`, async () => {
      const Page = load(path, {
        ...imports,
        "@/components/audit-view": { AuditView },
        "@/lib/bff": { getAudit: async () => { throw error; } },
      }).default;
      const html = renderToStaticMarkup(await Page());
      assert.match(html, /role="alert"/);
      assert.ok(html.includes(title));
      assert.ok(html.includes(detail));
      assert.doesNotMatch(html, /Audit substrate unreachable|Log verified|Receipts issued: 0/);
    });
  }

  test(`${path} retains successful empty evidence without treating it as a read failure`, async () => {
    const Page = load(path, {
      ...imports,
      "@/components/audit-view": { AuditView },
      "@/lib/bff": { getAudit: async () => ({
        receipts: [], inclusion_log_size: 0, checkpoint: null, integrity: null,
      }) },
    }).default;
    const html = renderToStaticMarkup(await Page());
    assert.match(html, /Receipts issued: 0/);
    assert.doesNotMatch(html, /role="alert"|Log verified/);
  });
}

const recordedAudit = (integrity) => ({
    receipts: [{ name: "receipt-run", namespace: "test", task: "run", inclusion_seq: 999, verdict: "verified" }],
    inclusion_log_size: 1,
    checkpoint: { tree_size: 1, root_hash: "root", key_id: "key" },
    integrity: { tree_size: 1, chain_consistent: true, checkpoint_verified: true, witness_present: false, anchor_pinned: false, ...integrity },
  });

  test("a verified log does not turn a receipt's recorded position into membership proof", () => {
    const html = renderToStaticMarkup(React.createElement(AuditView, { audit: recordedAudit(), error: null }));
    assert.match(html, /Log verified/);
    assert.match(html, /does not verify each listed receipt/);
    assert.match(html, /a position number is a recorded pointer, not proof of membership/);
    assert.match(html, /This is not an independent trust anchor/);
    assert.doesNotMatch(html, /All 1 receipts sit|position number proves|The run never exceeded/);
  });

  test("pinned log signatures do not assert freshness or independent witness verification", () => {
    const html = renderToStaticMarkup(React.createElement(AuditView, {
      audit: recordedAudit({ anchor_pinned: true, witness_present: true }), error: null,
    }));
    assert.match(html, /Pinned signature verified/);
    assert.match(html, /not its freshness or protection against replay of an older signed log/);
    assert.match(html, /identity and co-signature are not verified here/);
    assert.doesNotMatch(html, /no entry can be altered, removed, or forked without detection/);
  });

  for (const integrity of [
    { chain_consistent: false },
    { checkpoint_verified: false },
    { tree_size: 0 },
  ]) {
    test(`recorded checkpoint and position never override failed integrity: ${JSON.stringify(integrity)}`, () => {
      const html = renderToStaticMarkup(React.createElement(AuditView, {
        audit: recordedAudit(integrity), error: null,
      }));
      assert.doesNotMatch(html, /Log verified|Consistent \+ signed|Pinned signature verified/);
      assert.match(html, /Log not fully verified/);
    });
  }

  test("receipt positions and log verification remain separate when a position is missing", () => {
    const audit = recordedAudit();
    audit.receipts[0].inclusion_seq = null;
    const html = renderToStaticMarkup(React.createElement(AuditView, { audit, error: null }));
    assert.match(html, /Log verified/);
    assert.match(html, /1 receipt\(s\) lack a recorded inclusion-log position/);
    assert.match(html, /does not verify each listed receipt/);
  });

  const { receiptClaimVerdict, AuditReceiptRow } = load("../src/app/console/audit/audit-receipt-row.tsx", {
    "react": React,
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/components/compliance-pack": { CompliancePackView: empty },
  });
  const coreClasses = ["integrity", "conformance", "completeness"];
  const claim = (className, status = "PASS") => ({ class: className, status, detail: "" });

  for (const required of coreClasses) {
    test(`claim summary requires ${required}`, () => {
      assert.equal(receiptClaimVerdict(coreClasses.filter((c) => c !== required).map((c) => claim(c))), "partial");
    });
    for (const status of ["OMITTED", "PARTIAL", "", "UNKNOWN"]) {
      test(`claim summary retains ${required} ${status || "empty status"} as incomplete`, () => {
        assert.equal(receiptClaimVerdict(coreClasses.map((c) => claim(c, c === required ? status : "OK"))), "partial");
      });
    }
  }

  test("claim summary preserves advisory regulatory maturity, failure priority and empty evidence", () => {
    const core = coreClasses.map((c) => claim(c));
    for (const status of ["OMITTED", "PARTIAL"]) {
      assert.equal(receiptClaimVerdict([...core, claim("regulatory", status)]), "verified");
    }
    assert.equal(receiptClaimVerdict(coreClasses.map((c) => claim(c.toUpperCase(), "ok"))), "verified");
    assert.equal(receiptClaimVerdict([...core, claim("completeness", "OMITTED")]), "partial");
    assert.equal(receiptClaimVerdict([...core, claim("regulatory", "ERROR")]), "failed");
    assert.equal(receiptClaimVerdict([claim("unknown")]), "partial");
    assert.equal(receiptClaimVerdict([claim("integrity")]), "partial");
    assert.equal(receiptClaimVerdict([]), "none");
  });

  test("collapsed receipt labels passing claims without asserting cryptographic verification", () => {
    const html = renderToStaticMarkup(React.createElement(AuditReceiptRow, {
      ns: "test", task: "run", summaryTask: "run", summarySeq: 999, summaryVerdict: "verified",
    }));
    assert.match(html, /Claims pass/);
    assert.doesNotMatch(html, /Verified|Cryptographic checks passed/);
  });

  test("receipt summary dates agree across server/browser locales and timezones", () => {
    const render = (locale, timeZone) => {
      class EnvironmentDate extends Date {
        toLocaleDateString(locales, options) {
          return super.toLocaleDateString(locales ?? locale, { timeZone, ...options });
        }
      }
      const { AuditReceiptRow: Row } = load("../src/app/console/audit/audit-receipt-row.tsx", {
        "react": React,
        "react/jsx-runtime": require("react/jsx-runtime"),
        "@/components/compliance-pack": { CompliancePackView: empty },
      }, { Date: EnvironmentDate });
      return renderToStaticMarkup(React.createElement(Row, {
        ns: "test", task: "run", summaryTask: "run", summarySeq: 1,
        summaryVerdict: "partial", summaryCreated: "2026-10-13T00:30:00Z",
      }));
    };
    const server = render("en-US", "UTC");
    for (const [locale, zone] of [["en-GB", "Europe/London"], ["de-DE", "America/Los_Angeles"], ["ja-JP", "Asia/Tokyo"]]) {
      assert.equal(render(locale, zone), server);
    }
    assert.match(server, /13 Oct UTC/);
    assert.match(server, /dateTime="2026-10-13T00:30:00Z"/i);
    assert.match(server, /title="2026-10-13T00:30:00Z"/);
  });

  test("missing receipt dates remain absent rather than inventing a date", () => {
    for (const summaryCreated of [undefined, null, ""]) {
      const html = renderToStaticMarkup(React.createElement(AuditReceiptRow, {
        ns: "test", task: "run", summaryTask: "run", summarySeq: 1,
        summaryVerdict: "partial", summaryCreated,
      }));
      assert.doesNotMatch(html, /<time|Invalid Date| UTC/);
    }
  });

  test("expanded evidence distinguishes formatted JSON from the original signed bytes", () => {
    const receipt = {
      namespace: "test", task: "run", claims: coreClasses.map((c) => claim(c)),
      scheme: "dsse-ed25519", key_id: "published-key", inclusion_seq: 0,
    };
    const result = {
      verified: true,
      checks: [],
      evidence: { signed_statement: { predicate: { message: "retained content" } } },
    };
    const states = [true, receipt, null, null, false, false, null, false, result, null];
    let stateIndex = 0;
    const { AuditReceiptRow: ExpandedRow } = load("../src/app/console/audit/audit-receipt-row.tsx", {
      "react": { ...React, useState: () => [states[stateIndex++], () => {}] },
      "react/jsx-runtime": require("react/jsx-runtime"),
      "@/components/compliance-pack": { CompliancePackView: empty },
    });
    const html = renderToStaticMarkup(React.createElement(ExpandedRow, {
      ns: "test", task: "run", summaryTask: "run", summarySeq: 0, summaryVerdict: "verified",
    }));
    assert.equal(stateIndex, states.length);
    assert.match(html, /Signed statement \(decoded, formatted JSON\)/);
    assert.match(html, /preserve the JSON content, not the original signed bytes/);
    assert.match(html, /checks the retained DSSE payload and signature/);
    assert.match(html, /retained content/);
    assert.match(html, /Download JSON/);
    assert.match(html, /Cryptographic checks passed/);
    assert.match(html, /this does not prove control enforcement/);
    assert.doesNotMatch(html, /the exact bytes the signature covers/);
  });

    for (const status of [404, 403, 503, "network", "invalid-json"]) {
      test(`compliance ${status} stays visible and can retry without invalidating receipt verification`, async () => {
        const receipt = {
          namespace: "test", task: "run", claims: coreClasses.map((c) => claim(c)),
          scheme: "dsse-ed25519", key_id: "published-key", inclusion_seq: 0,
        };
        const states = [false, null, null, null, false, false, null, false, null, null];
        const requests = [];
        let stateIndex = 0, retry = false;
        const { AuditReceiptRow: Row } = load("../src/app/console/audit/audit-receipt-row.tsx", {
          "react": { ...React, useState: () => {
            const index = stateIndex++;
            return [states[index], (value) => { states[index] = value; }];
          } },
          "react/jsx-runtime": require("react/jsx-runtime"),
          "@/components/compliance-pack": { CompliancePackView: () => React.createElement("p", null, "Compliance pack loaded") },
        }, {
          fetch: async (url) => {
            requests.push(url);
            if (url.endsWith("/receipt")) return { ok: true, status: 200, json: async () => receipt };
            assert.match(url, /\/compliance$/);
            if (retry) return { ok: true, status: 200, json: async () => ({}) };
            if (status === "network") throw new Error("private transport detail");
            if (status === "invalid-json") return { ok: true, status: 200, json: async () => { throw new Error("private body"); } };
            return { ok: false, status };
          },
        });
        const props = { ns: "test", task: "run", summaryTask: "run", summarySeq: 0, summaryVerdict: "verified" };
        const render = () => { stateIndex = 0; return Row(props); };
        const buttons = (node) => {
          if (!node || typeof node !== "object") return [];
          if (Array.isArray(node)) return node.flatMap(buttons);
          return [...(node.type === "button" ? [node] : []), ...buttons(node.props?.children)];
        };
        await buttons(render())[0].props.onClick();
        const tree = render();
        const html = renderToStaticMarkup(tree);
        assert.match(html, /role="alert"/);
        assert.match(html, status === 404 ? /No compliance mapping is available/ : /Compliance mapping couldn&#x27;t be loaded/);
        assert.match(html, /Verify now/);
        assert.doesNotMatch(html, /private transport detail|private body|Compliance pack loaded/);
        retry = true;
        await buttons(tree).find(button => button.props.children === "Retry compliance mapping").props.onClick();
        const recovered = renderToStaticMarkup(render());
        assert.doesNotMatch(recovered, /role="alert"|Retry compliance mapping/);
        assert.match(recovered, /Compliance pack loaded/);
        assert.match(recovered, /Verify now/);
        assert.equal(requests.filter(url => url.endsWith("/receipt")).length, 1);
        assert.equal(requests.filter(url => url.endsWith("/compliance")).length, 2);
      });
    }
