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

function load(path, imports) {
  const context = {
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
