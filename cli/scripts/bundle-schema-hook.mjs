// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { createRequire, isBuiltin } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "tsdown";
import ts from "typescript";

const cli = fileURLToPath(new URL("../", import.meta.url));
const root = resolve(cli, "..");
const output = resolve(root, "deploy/helm/kars/files");
const args = process.argv.slice(2);
if (args.length > 1 || (args.length === 1 && args[0] !== "--check")) throw new Error("Only --check is supported");
const check = args[0] === "--check";
const require = createRequire(import.meta.url);
const yamlPackage = require.resolve("yaml/package.json");
const installedYaml = JSON.parse(await readFile(yamlPackage, "utf8")).version;
const lockedYaml = JSON.parse(await readFile(resolve(cli, "package-lock.json"), "utf8"))
  .packages?.["node_modules/yaml"]?.version;
if (!lockedYaml || installedYaml !== lockedYaml) {
  throw new Error(`Schema helper requires lockfile yaml ${lockedYaml ?? "(missing)"}; found ${installedYaml}. Restore locked CLI dependencies before bundling.`);
}
const stage = await mkdtemp(resolve(cli, ".schema-hook-build-"));
try {
  await build({
    cwd: root, config: false, entry: [resolve(cli, "src/schema-hook.ts")],
    outDir: stage, clean: false, dts: false, format: "esm", platform: "node",
    target: "node22", minify: true, sourcemap: false, logLevel: "error",
    deps: { alwaysBundle: ["yaml"] },
    outputOptions: { entryFileNames: "schema-hook.mjs" },
  });
  const files = await readdir(stage);
  if (files.length !== 1 || files[0] !== "schema-hook.mjs") throw new Error("Schema helper must be a single executable bundle");
  const code = "// Copyright (c) Microsoft Corporation.\n// Licensed under the MIT License.\n"
    + await readFile(resolve(stage, files[0]), "utf8");
  if (Buffer.byteLength(code) > 900_000) throw new Error("Schema helper exceeds its ConfigMap bound");
  const parsed = ts.createSourceFile("schema-hook.mjs", code, ts.ScriptTarget.Latest, true, ts.ScriptKind.JS);
  const visit = node => {
    if (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) {
      if (node.moduleSpecifier && (!ts.isStringLiteral(node.moduleSpecifier) || !isBuiltin(node.moduleSpecifier.text))) {
        throw new Error("Schema helper has an external runtime dependency");
      }
    }
    if (ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword
      || (ts.isIdentifier(node.expression) && node.expression.text === "require"))) {
      throw new Error("Schema helper cannot load runtime modules dynamically");
    }
    ts.forEachChild(node, visit);
  };
  visit(parsed);
  const notice = await readFile(resolve(dirname(yamlPackage), "LICENSE"), "utf8");
  for (const [name, content] of [["schema-hook.mjs", code], ["schema-hook.NOTICE", `Bundled dependency: yaml\n\n${notice}`]]) {
    if (check) {
      if (await readFile(resolve(output, name), "utf8") !== content) throw new Error(`Generated ${name} is stale`);
    } else {
      await writeFile(resolve(output, name), content);
    }
  }
} finally {
  await rm(stage, { recursive: true, force: true });
}
