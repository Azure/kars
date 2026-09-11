// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { createRequire } from "node:module";
import { basename } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const require = createRequire(import.meta.url);
const ts = require("typescript");
const entry = fileURLToPath(new URL("../src/lib/types.ts", import.meta.url));
const clientContracts = fileURLToPath(new URL("../src/lib/bff-contracts.ts", import.meta.url));
const directory = new URL("../src/lib/types/", import.meta.url);
const modules = readdirSync(directory).filter(name => name.endsWith(".ts"))
  .map(name => fileURLToPath(new URL(name, directory)));
const lock = JSON.parse(readFileSync(new URL("../package-lock.json", import.meta.url), "utf8"));

test("the shared DTO surface type-checks with its locked compiler", () => {
  assert.equal(ts.version, lock.packages["node_modules/typescript"].version);
  const program = ts.createProgram([entry, ...modules, clientContracts], {
    strict: true, noEmit: true, types: [], skipLibCheck: true,
    target: ts.ScriptTarget.ES2017,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    lib: ["lib.esnext.d.ts", "lib.dom.d.ts"],
  });
  const diagnostics = ts.getPreEmitDiagnostics(program);
  assert.deepEqual(diagnostics.map(diagnostic =>
    ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n")), []);
  const checker = program.getTypeChecker();
  const specifiers = new Set(modules.map(path => `./types/${basename(path, ".ts")}`));
  for (const path of [entry, ...modules]) {
    for (const statement of program.getSourceFile(path).statements) {
      if (path === entry) {
        assert.ok(ts.isExportDeclaration(statement) && statement.moduleSpecifier
          && ts.isStringLiteral(statement.moduleSpecifier)
          && specifiers.has(statement.moduleSpecifier.text),
        "the DTO barrel may only forward the known domain modules");
      } else if (ts.isImportDeclaration(statement)) {
        assert.ok(statement.importClause?.isTypeOnly, "domain dependencies must be erased type imports");
      } else if (ts.isVariableStatement(statement)) {
        assert.ok(statement.declarationList.flags & ts.NodeFlags.Const);
        for (const declaration of statement.declarationList.declarations) {
          assert.ok(ts.isIdentifier(declaration.name)
            && ["TIER_LABELS", "WIRING_LABELS"].includes(declaration.name.text));
          assert.ok(declaration.initializer && ts.isObjectLiteralExpression(declaration.initializer)
            && declaration.initializer.properties.every(property =>
              ts.isPropertyAssignment(property) && !ts.isComputedPropertyName(property.name)
              && ts.isStringLiteral(property.initializer)),
          "label values must remain inert string-literal objects");
        }
      } else {
        assert.ok(ts.isInterfaceDeclaration(statement) || ts.isTypeAliasDeclaration(statement),
          "domain modules must not introduce runtime side effects");
      }
    }
  }
  const exports = path => checker.getExportsOfModule(
    checker.getSymbolAtLocation(program.getSourceFile(path)),
  );
  const domainNames = modules.flatMap(path => exports(path).map(symbol => symbol.name));
  assert.equal(new Set(domainNames).size, domainNames.length, "public DTO names must not collide");
  assert.deepEqual(exports(entry).map(symbol => symbol.name).sort(), domainNames.sort(),
    "the original barrel must retain every public domain export");
  const runtimeNames = exports(entry).filter(symbol => {
    const target = symbol.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(symbol) : symbol;
    return target.flags & ts.SymbolFlags.Value;
  }).map(symbol => symbol.name).sort();
  assert.deepEqual(runtimeNames, ["TIER_LABELS", "WIRING_LABELS"],
    "DTO modules must not add runtime initialization or lose existing label exports");
  assert.ok(program.getSourceFile(clientContracts).statements.every(statement =>
    ts.isInterfaceDeclaration(statement) || ts.isTypeAliasDeclaration(statement)),
  "server client contracts must remain entirely erased types");
});

test("server client types keep their original barrel without moving transport", () => {
  const path = fileURLToPath(new URL("../src/lib/bff.ts", import.meta.url));
  const source = ts.createSourceFile(path, readFileSync(path, "utf8"), ts.ScriptTarget.Latest, true);
  const forwarding = source.statements.filter(statement =>
    ts.isExportDeclaration(statement) && statement.isTypeOnly && !statement.exportClause
    && statement.moduleSpecifier?.text === "./bff-contracts");
  assert.equal(forwarding.length, 1);
  assert.ok(source.statements.some(statement =>
    ts.isFunctionDeclaration(statement) && statement.name?.text === "authenticatedBffFetch"));
  assert.ok(source.statements.some(statement =>
    ts.isClassDeclaration(statement) && statement.name?.text === "BffError"));
});

test("the public DTO barrel and domain modules remain bounded", () => {
  assert.ok(modules.length > 0);
  for (const path of [entry, ...modules, clientContracts]) {
    const source = readFileSync(path, "utf8");
    const lines = source.split("\n").length - (source.endsWith("\n") ? 1 : 0);
    assert.ok(lines <= 800, `${path} has ${lines} physical lines`);
  }
});
