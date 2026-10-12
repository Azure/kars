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
const file = new URL("../src/app/workspace/teams/[name]/page.tsx", import.meta.url);
const source = ts.createSourceFile(file.pathname, readFileSync(file, "utf8"),
  ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
class BffError extends Error {
  constructor(status) { super("private upstream detail"); this.status = status; }
}

function load(text, globals = {}) {
  const exports = {};
  runInNewContext(ts.transpileModule(text, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText, {
    exports, ...globals,
    require: name => name === "@/lib/bff" ? { BffError }
      : name === "@/components/deliverable-view"
        ? { DeliverableBody: ({ output }) => React.createElement("p", null, output) }
        : require(name),
  });
  return exports;
}
const { TeamReadError } = load(readFileSync(
  new URL("../src/app/workspace/teams/read-error.tsx", import.meta.url), "utf8"));
const retainedSource = readFileSync(new URL(
  "../src/components/retained-team-documents.tsx", import.meta.url), "utf8");
const { RetainedTeamDocuments } = load(retainedSource);
let readStatement;
const sections = {};
function visit(node) {
  if (ts.isVariableStatement(node) && node.declarationList.declarations.some(
    d => d.name.getText(source) === "[commonsRead, ledgerRead]",
  )) readStatement = node.getText(source);
  if (ts.isObjectLiteralExpression(node)) {
    const id = node.properties.find(p => p.name?.getText(source) === "id")?.initializer?.text;
    if (id === "knowledge" || id === "ledger") {
      const content = node.properties.find(p => p.name?.getText(source) === "node").initializer;
      function section(child) {
        if (!sections[id] && ts.isJsxElement(child)
            && child.openingElement.tagName.getText(source) === "section") {
          sections[id] = child.getText(source);
        }
        ts.forEachChild(child, section);
      }
      section(content);
    }
  }
  ts.forEachChild(node, visit);
}
visit(source);
assert.ok(readStatement && sections.knowledge && sections.ledger);
async function reads(commons, ledger) {
  return load(`export async function read() {
    ${readStatement}
    return { commonsRead, ledgerRead };
  }`, {
    ns: "tenant-a", name: "clinic",
    getTeamCommons: async () => { if (commons instanceof Error) throw commons; return commons; },
    getTeamLedger: async () => { if (ledger instanceof Error) throw ledger; return ledger; },
  }).read();
}
function render(section, state) {
  const component = load(`export function View() { return (${sections[section]}); }`, {
    ...state,
    commons: state.commonsRead.status === "fulfilled" ? state.commonsRead.value : null,
    team: { name: "clinic" }, TeamReadError, RetainedTeamDocuments,
    Link: ({ children, href }) => React.createElement("a", { href }, children),
    DeliverableBody: ({ output }) => React.createElement("p", null, output),
    TeamLedger: () => React.createElement("p", null, "retained ledger"),
    toPlainPreview: content => content,
  });
  return renderToStaticMarkup(React.createElement(component.View));
}

for (const status of [404, 403, 500]) {
  test(`unverified memory is not reported as zero entries (${status})`, async () => {
    const state = await reads(new BffError(status), []);
    const html = render("knowledge", state);
    assert.match(html, /role="alert"/);
    assert.match(html, /Unverified/);
    assert.doesNotMatch(html, /No shared knowledge yet|0 entries|private upstream detail/);
    assert.match(render("ledger", state), /No activity recorded yet/);
  });
}
test("verified empty memory stays distinct from a failed diagnostic ledger", async () => {
  const state = await reads({ count: 0, entries: [] }, new BffError(500));
  assert.match(render("knowledge", state), /No shared knowledge yet/);
  assert.match(render("knowledge", state), /0 entries/);
  assert.match(render("ledger", state), /role="alert"/);
  assert.doesNotMatch(render("ledger", state), /No activity recorded yet/);
});
test("memory exposes stored provenance separately from readable-content identity", async () => {
  const state = await reads({ count: 1, entries: [{
    id: "one", title: "Decision", author: "clinic-run-one", source_task: "clinic-run-one",
    created_at: "2026-10-12T08:00:00Z", content: "Complete retained text",
    digest: "sha256:displayed", stored_digest: "sha256:stored", stored_size_bytes: 123,
  }] }, []);
  const html = render("knowledge", state);
  assert.match(html, /1 entries/);
  assert.match(html, /Complete retained text/);
  assert.match(html, /sha256:displayed/);
  assert.match(html, /Stored record:.*sha256:stored.*123 bytes/);
  assert.doesNotMatch(html, /No shared knowledge yet|Unverified/);
});

test("memory exposes complete documents, peer provenance and exact-byte downloads", async () => {
  const content = "# Decision\r\nKeep complete bytes — 日本語 🌍\n";
  const file = { name: "decision-log.md", content, sha256: "sha256:full-file",
    source: { artifactName: "decision-log.md", taskName: "clinic-run", taskUid: "original-uid",
      runNonce: "original-nonce", assignmentId: "assignment-one", agentDid: "did:mesh:original" } };
  const archive = { documents: [file], handoffs: [{ ...file, name: "r0-p1.md",
    source: { ...file.source, artifactName: "handoff.md", taskName: "reviewer" } }] };
  const state = await reads({ count: 1, entries: [{
    id: "one", title: "Decision", author: "clinic-run", source_task: "clinic-run",
    created_at: "2026-10-12T08:00:00Z", content: "Completed", digest: "sha256:summary", archive,
  }] }, []);
  const html = render("knowledge", state);
  for (const text of ["Retained documents", "Peer handoffs", "decision-log.md", "handoff.md",
    "Keep complete bytes", "original-uid", "original-nonce", "assignment-one", "sha256:full-file"]) {
    assert.ok(html.includes(text), text);
  }
  const ast = ts.createSourceFile("retained.tsx", retainedSource, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const download = ast.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === "downloadRetainedDocument");
  const link = { click() { this.clicked = true; }, remove() { this.removed = true; } };
  let blob;
  let revoked;
  const api = load(`export ${download.getText(ast)}`, {
    Blob, document: { createElement: () => link, body: { appendChild() {} } },
    URL: { createObjectURL(value) { blob = value; return "blob:retained"; },
      revokeObjectURL(value) { revoked = value; } },
  });
  api.downloadRetainedDocument(file);
  assert.equal(await blob.text(), content);
  assert.equal(link.download, file.name);
  assert.equal(link.href, "blob:retained");
  assert.ok(link.clicked && link.removed);
  assert.equal(revoked, "blob:retained");
});
