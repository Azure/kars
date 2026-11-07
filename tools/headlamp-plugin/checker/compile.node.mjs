// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.dirname(here);
const require = createRequire(import.meta.url);
const readJson = file => JSON.parse(readFileSync(file, 'utf8'));

test('installed Storybook checker compiles valid TypeScript, reports TS2322, and recovers', async t => {
  const provenance = readJson(path.join(here, 'upstream.json'));
  const checkerEntry = require.resolve('fork-ts-checker-webpack-plugin');
  const installed = path.resolve(path.dirname(checkerEntry), '..');
  const manifest = readJson(path.join(installed, 'package.json'));
  assert.deepEqual(manifest.karsPatch?.upstream, provenance,
    'A clean npm ci must install the patched checker before this compiler regression can run');
  assert.equal(manifest.name, provenance.name);
  assert.equal(manifest.version, provenance.version);
  assert.equal(manifest.dependencies['deepmerge-ts'], provenance.replacement.version);
  assert(!Object.hasOwn(manifest.dependencies, 'deepmerge'));
  const adapter = readFileSync(path.join(installed, 'lib/merge-options.cjs'));
  assert.deepEqual(adapter, readFileSync(path.join(here, 'merge-options.cjs')));
  assert.equal(createHash('sha256').update(adapter).digest('hex'), manifest.karsPatch.mergeOptionsSha256);
  const consumer = readFileSync(path.join(installed, 'lib/plugin.js'), 'utf8');
  assert(consumer.includes('const mergeOptions = require("./merge-options.cjs");'));
  assert(consumer.includes('this.options = mergeOptions(externalOptions || {}, options || {});'));
  const checkerRequire = createRequire(checkerEntry);
  const mergerManifest = path.resolve(path.dirname(checkerRequire.resolve('deepmerge-ts')), '..', 'package.json');
  assert.equal(readJson(mergerManifest).version, provenance.replacement.version);
  const storybookRequire = createRequire(require.resolve('@storybook/builder-webpack5'));
  assert.equal(storybookRequire.resolve('fork-ts-checker-webpack-plugin'), checkerEntry,
    'Storybook must use the same installed patched checker');

  const webpack = storybookRequire('webpack');
  const Checker = checkerRequire(checkerEntry);
  const fixture = path.join(root, `.checker-compile-${randomUUID()}`);
  mkdirSync(fixture);
  t.after(() => rmSync(fixture, { recursive: true, force: true }));
  const source = path.join(fixture, 'index.ts');
  const configFile = path.join(fixture, 'tsconfig.json');
  writeFileSync(configFile, JSON.stringify({
    compilerOptions: {
      strict: true,
      target: 'ES2020',
      module: 'ESNext',
      moduleResolution: 'node',
      lib: ['ES2020'],
      types: [],
    },
    files: ['index.ts'],
  }));

  async function compile(contents) {
    writeFileSync(source, contents);
    const compiler = webpack({
      mode: 'development',
      context: fixture,
      entry: source,
      output: { path: path.join(fixture, 'dist'), filename: 'bundle.js' },
      cache: false,
      devtool: false,
      module: {
        rules: [{
          test: /\.ts$/,
          loader: require.resolve('ts-loader'),
          options: { configFile },
        }],
      },
      plugins: [new Checker({
        async: false,
        typescript: {
          configFile,
          context: fixture,
          typescriptPath: require.resolve('typescript'),
          memoryLimit: 512,
          diagnosticOptions: { syntactic: true, semantic: true, declaration: true, global: true },
        },
      })],
    });
    const issues = [];
    const workerErrors = [];
    let issueCallbacks = 0;
    const hooks = Checker.getCompilerHooks(compiler);
    // Observe real worker results without filtering or replacing diagnostics.
    hooks.issues.tap('KarsCheckerCompilerRegression', reported => {
      issueCallbacks++;
      issues.push(...reported);
      return reported;
    });
    hooks.error.tap('KarsCheckerCompilerRegression', error => workerErrors.push(error));
    try {
      const stats = await new Promise((resolve, reject) => {
        compiler.run((error, result) => error ? reject(error) : resolve(result));
      });
      assert(stats, 'Webpack must return compilation statistics');
      assert.deepEqual(workerErrors, [], 'Checker workers must not fail silently');
      assert(issueCallbacks > 0, 'The actual checker worker must report completion on every compilation');
      return { stats, issues };
    } finally {
      await new Promise((resolve, reject) => {
        compiler.close(error => error ? reject(error) : resolve());
      });
    }
  }

  function assertValid(result) {
    assert.equal(result.stats.hasErrors(), false, result.stats.toString({ all: false, errors: true }));
    assert.equal(result.stats.hasWarnings(), false, result.stats.toString({ all: false, warnings: true }));
    assert.deepEqual(result.issues, []);
    assert(result.stats.compilation.getAsset('bundle.js'), 'A valid project must produce its bundle');
  }

  const valid = 'export const answer: number = 42;\n';
  await t.test('valid tiny project compiles through Webpack and the checker worker', async () => {
    assertValid(await compile(valid));
  });
  await t.test('invalid assignment fails compilation with checker TS2322 in index.ts', async () => {
    const result = await compile('export const answer: number = "not a number";\n');
    assert.equal(result.stats.hasErrors(), true, 'The intentional type error must fail compilation');
    const matches = issue => issue?.code === 'TS2322' && issue.severity === 'error' &&
      typeof issue.file === 'string' && path.resolve(fixture, issue.file) === source;
    assert(result.issues.some(matches), 'The checker worker must report TS2322 for the changed source file');
    assert(result.stats.compilation.errors.some(error => matches(error.issue)),
      'The checker diagnostic must reach Webpack errors, not merely the loader or an observation hook');
  });
  await t.test('fixing the same source file recovers with a fresh compiler', async () => {
    assertValid(await compile(valid));
  });
});
