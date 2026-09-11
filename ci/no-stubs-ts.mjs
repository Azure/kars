// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";

const require = createRequire(new URL("../cli/package.json", import.meta.url));
const ts = require("typescript");
const [base, file, patterns] = process.argv.slice(2);
if (!base || !file || !patterns) {
  throw new Error("fail: source gate requires a base revision, file and marker patterns");
}

const source = execFileSync("git", ["show", `HEAD:${file}`], { encoding: "utf8" });
const diff = execFileSync("git", ["diff", "--unified=0", `${base}...HEAD`, "--", file],
  { encoding: "utf8" });
const kind = file.endsWith(".tsx") ? ts.ScriptKind.TSX
  : /\.(?:jsx?|mjs|cjs)$/.test(file) ? ts.ScriptKind.JSX : ts.ScriptKind.TS;
const tree = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, kind);
if (tree.parseDiagnostics.length) {
  const diagnostic = tree.parseDiagnostics[0];
  const position = tree.getLineAndCharacterOfPosition(diagnostic.start ?? 0);
  throw new Error(`fail: ${file}:${position.line + 1}: cannot parse source for marker analysis: ${
    ts.flattenDiagnosticMessageText(diagnostic.messageText, " ")}`);
}

const ranges = [];
function isPlaceholderField(node) {
  const parent = node.parent;
  if (!parent) return false;
  if ((ts.isPropertyAssignment(parent) || ts.isPropertySignature(parent)
    || ts.isPropertyDeclaration(parent) || ts.isJsxAttribute(parent)
    || ts.isShorthandPropertyAssignment(parent) || ts.isPropertyAccessExpression(parent))
    && parent.name === node) return true;
  if (ts.isBindingElement(parent) && (parent.name === node || parent.propertyName === node)) {
    return true;
  }
  if (!ts.isIdentifier(node)) return false;
  for (let ancestor = parent; ancestor; ancestor = ancestor.parent) {
    if (ts.isFunctionLike(ancestor) || ts.isClassLike(ancestor)
      || ts.isVariableDeclaration(ancestor)) return false;
    if (ts.isJsxAttribute(ancestor)) return ancestor.name.getText(tree) === "placeholder";
    if (ts.isPropertyAssignment(ancestor)) {
      return (ts.isIdentifier(ancestor.name) || ts.isStringLiteral(ancestor.name))
        && ancestor.name.text === "placeholder";
    }
    if (ts.isStatement(ancestor)) return false;
  }
  return false;
}

function visit(node) {
  if ((ts.isIdentifier(node) || ts.isStringLiteral(node))
    && node.text === "placeholder" && isPlaceholderField(node)) {
    ranges.push([node.getStart(tree), node.getEnd()]);
  }
  if (ts.isStringLiteral(node) && ts.isJsxAttribute(node.parent)
    && node.parent.name.getText(tree) === "className") {
    const start = node.getStart(tree) + 1;
    const text = source.slice(start, node.getEnd() - 1);
    for (const match of text.matchAll(/(^|[:\s])placeholder(?=:(?:-?[A-Za-z]|\[))/g)) {
      const offset = start + match.index + match[1].length;
      ranges.push([offset, offset + "placeholder".length]);
    }
  }
  ts.forEachChild(node, visit);
}
visit(tree);

// Ignore field syntax, not whole lines: comments and string values still
// carry unfinished-implementation markers even alongside a legitimate UI prop.
const parts = [];
let cursor = 0;
for (const [start, end] of ranges.sort((a, b) => a[0] - b[0])) {
  if (start < cursor) throw new Error(`fail: ${file}: overlapping syntax ranges`);
  parts.push(source.slice(cursor, start), " ".repeat(end - start));
  cursor = end;
}
parts.push(source.slice(cursor));
const original = source.split("\n");
const masked = parts.join("").split("\n");
const markers = new RegExp(patterns);
let lineNumber;
for (const line of diff.split("\n")) {
  const hunk = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(line);
  if (hunk) {
    lineNumber = Number(hunk[1]) - 1;
  } else if (lineNumber !== undefined && line.startsWith("+")) {
    const added = line.slice(1);
    if (original[lineNumber] !== added) {
      throw new Error(`fail: ${file}: diff does not match committed source at line ${lineNumber + 1}`);
    }
    if (line.length > 1 && line[1] !== "+" && !added.includes("ci:stub-ok:")
      && markers.test(masked[lineNumber])) {
      console.error(`fail: ${file}: new stub/placeholder introduced: ${
        Array.from(added).slice(0, 160).join("")}`);
      process.exitCode = 1;
    }
    lineNumber += 1;
  } else if (lineNumber !== undefined && line.startsWith(" ")) {
    lineNumber += 1;
  }
}
