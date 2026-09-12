import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const path = new URL("../src/app/workspace/teams/[name]/runs/[run]/page.tsx", import.meta.url);
const source = ts.createSourceFile(path.pathname, readFileSync(path, "utf8"),
  ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
const names = ["githubPullRequests", "archivedPullRequests"];
const functions = source.statements.filter(statement =>
  ts.isFunctionDeclaration(statement) && names.includes(statement.name?.text));
assert.equal(functions.length, names.length);
const context = { exports: {}, URL };
const helpers = functions.map(statement => statement.getText(source)).join("\n");
runInNewContext(ts.transpileModule(`${helpers}\nexport { ${names.join(", ")} };`, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText, context);
const links = (text, repos = []) =>
  JSON.parse(JSON.stringify(context.exports.archivedPullRequests(text, repos)));
const expected = [{ repo: "Azure/kars", number: 563, url: "https://github.com/Azure/kars/pull/563" }];

test("live claim warnings and archive links use the same host-checked parser", () => {
  let claim;
  function visit(node) {
    if (ts.isVariableDeclaration(node) && node.name.getText(source) === "claimedPullRequest") claim = node;
    ts.forEachChild(node, visit);
  }
  visit(source);
  assert.ok(claim);
  assert.match(claim.initializer.getText(source), /githubPullRequests\(task\.result\.output\)/);
});

test("GitHub links in prose, markdown, and supported bare URLs retain canonical HTTPS output", () => {
  for (const text of [
    "Created https://github.com/Azure/kars/pull/563.",
    "[PR](https://github.com/Azure/kars/pull/563)",
    "<https://github.com/Azure/kars/pull/563/files?diff=split#review>",
    "`github.com/Azure/kars/pull/563`",
    "HTTP://GITHUB.COM/Azure/kars/pulls/563",
    "https://github.com:443/Azure/kars/pull/563",
  ]) assert.deepEqual(links(text), expected, text);
  assert.deepEqual(links("https://github.com/Azure/kars/pull/563 github.com/Azure/kars/pull/563"), expected);
});

test("domain substrings, credentials, other protocols, and malformed PR paths are not GitHub evidence", () => {
  for (const text of [
    "https://evilgithub.com/Azure/kars/pull/563",
    "https://github.com.evil.example/Azure/kars/pull/563",
    "https://evil.example/github.com/Azure/kars/pull/563",
    "https://evil.example/?next=github.com/Azure/kars/pull/563",
    "https://github.com@evil.example/Azure/kars/pull/563",
    "https://user:password@github.com/Azure/kars/pull/563",
    "https://github.com:8443/Azure/kars/pull/563",
    "ftp://github.com/Azure/kars/pull/563",
    "evilgithub.com/Azure/kars/pull/563",
    "https://github.com/Azure/kars/pull/563oops",
    "https://github.com/Azure/kars/pull/0",
    "https://github.com/Azure/kars/pull/9007199254740992",
  ]) assert.deepEqual(links(text), [], text);
});

test("PR shorthand remains tied to one valid configured repository", () => {
  assert.deepEqual(links("Delivered PR #563", ["Azure/kars"]), expected);
  assert.deepEqual(links("Delivered PR #563"), []);
  assert.deepEqual(links("Delivered PR #563", ["Azure/kars", "Azure/other"]), []);
  for (const repo of ["@evil.example/Azure/kars", "Azure/kars?next=evil", "Azure/kars/extra"]) {
    assert.deepEqual(links("Delivered PR #563", [repo]), []);
  }
  assert.deepEqual(links("PR #0, PR #9007199254740992", ["Azure/kars"]), []);
});
