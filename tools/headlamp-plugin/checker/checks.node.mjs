// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { x } from 'tar';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.dirname(here);
const require = createRequire(import.meta.url);
const readJson = filename => JSON.parse(readFileSync(filename, 'utf8'));
const manifest = readJson(path.join(root, 'package.json'));
const lock = readJson(path.join(root, 'package-lock.json'));
const provenance = readJson(path.join(here, 'upstream.json'));
const filename = 'fork-ts-checker-webpack-plugin-8.0.0-kars.tgz';
const archive = path.join(here, filename);
const checkerLock = lock.packages['node_modules/fork-ts-checker-webpack-plugin'];
const fixture = path.join(root, `.checker-test-${randomUUID()}`);
const checker = path.join(fixture, 'node_modules/fork-ts-checker-webpack-plugin');
let merge;

function verifyIntegrity(file, integrity) {
  const [algorithm, expected] = integrity.split('-');
  assert.equal(createHash(algorithm).update(readFileSync(file)).digest('base64'), expected);
}

before(() => {
  mkdirSync(checker, { recursive: true });
  verifyIntegrity(archive, checkerLock.integrity);
  x({ file: archive, cwd: checker, strip: 1, sync: true });
  if (process.env.KARS_CHECKER_MERGE_ARCHIVE) {
    const dependencyArchive = path.resolve(process.env.KARS_CHECKER_MERGE_ARCHIVE);
    verifyIntegrity(dependencyArchive, lock.packages['node_modules/deepmerge-ts'].integrity);
    const destination = path.join(fixture, 'node_modules/deepmerge-ts');
    mkdirSync(destination);
    x({ file: dependencyArchive, cwd: destination, strip: 1, sync: true });
    assert.equal(readJson(path.join(destination, 'package.json')).version, provenance.replacement.version);
  } else {
    const installed = path.resolve(path.dirname(require.resolve('deepmerge-ts')), '..', 'package.json');
    assert.equal(readJson(installed).version, provenance.replacement.version);
    const installedEntry = require.resolve('fork-ts-checker-webpack-plugin');
    const installedChecker = path.resolve(path.dirname(installedEntry), '..');
    assert.deepEqual(readJson(path.join(installedChecker, 'package.json')).karsPatch.upstream, provenance);
    assert.deepEqual(readFileSync(path.join(installedChecker, 'lib/plugin.js')), readFileSync(path.join(checker, 'lib/plugin.js')));
    const storybookRequire = createRequire(require.resolve('@storybook/builder-webpack5'));
    assert.equal(storybookRequire.resolve('fork-ts-checker-webpack-plugin'), installedEntry);
  }
  merge = require(path.join(checker, 'lib/merge-options.cjs'));
});
after(() => rmSync(fixture, { recursive: true, force: true }));

test('checker archive preserves immutable upstream identity, MIT attribution, workers and hooks', () => {
  const packaged = readJson(path.join(checker, 'package.json'));
  assert.equal(packaged.name, provenance.name);
  assert.equal(packaged.version, provenance.version);
  assert.equal(packaged.license, 'MIT');
  assert.equal(packaged.engines.node, '>=16.9.0');
  assert(!Object.hasOwn(packaged.dependencies, 'deepmerge'));
  assert.equal(packaged.dependencies['deepmerge-ts'], '8.0.2');
  assert.deepEqual(packaged.karsPatch.upstream, provenance);
  assert.deepEqual(readJson(path.join(checker, 'KARS-PATCH.json')), provenance);
  assert.equal(packaged.karsPatch.mergeOptionsSha256,
    createHash('sha256').update(readFileSync(path.join(here, 'merge-options.cjs'))).digest('hex'));
  assert.deepEqual(readFileSync(path.join(checker, 'lib/merge-options.cjs')), readFileSync(path.join(here, 'merge-options.cjs')));
  assert.deepEqual(readFileSync(path.join(checker, 'KARS-LICENSE')), readFileSync(path.join(here, 'LICENSE')));
  for (const [file, expected] of Object.entries({
    'LICENSE': 'bdd70f07d7646609b0b8f63a48eba2fbb11379cc288de55ff286b4679b5a9d4f',
    'lib/index.js': '5d3e22bf04262ce099cf982e9358c9e27a4b81fc5c8c36a743cee95c07975f50',
    'lib/typescript/worker/get-issues-worker.js': '7be986c557593ba89637cbfed55129b353f683f51af1ccbd0fc8adc55f6cf751',
    'lib/typescript/worker/get-dependencies-worker.js': 'c7f6b4e08b8c124042e39148f720017144b1fb663d895b1b5737f08a70f1397e',
  })) {
    assert.equal(createHash('sha256').update(readFileSync(path.join(checker, file))).digest('hex'), expected, file);
  }
  const originalConsumer = readFileSync(path.join(checker, 'lib/plugin.js'), 'utf8')
    .replace('const mergeOptions = require("./merge-options.cjs");', 'const deepmerge_1 = __importDefault(require("deepmerge"));')
    .replace('this.options = mergeOptions(externalOptions || {}, options || {});',
      'this.options = (0, deepmerge_1.default)(externalOptions || {}, options || {});');
  assert.equal(createHash('sha256').update(originalConsumer).digest('hex'),
    'e63c2eed9a013a5e21a1de85473cb3d33a674c85bdabea425a85d9b56b2ff0ec');
});

test('one real local checker replaces deepmerge without dropping the Storybook dependency path', () => {
  const spec = `file:checker/${filename}`;
  assert.equal(manifest.devDependencies['fork-ts-checker-webpack-plugin'], spec);
  assert.equal(manifest.overrides['@storybook/builder-webpack5']['fork-ts-checker-webpack-plugin'], '$fork-ts-checker-webpack-plugin');
  assert.equal(lock.packages[''].devDependencies['fork-ts-checker-webpack-plugin'], spec);
  assert.equal(checkerLock.resolved, spec);
  assert.deepEqual(Object.keys(lock.packages).filter(key => key.endsWith('/fork-ts-checker-webpack-plugin')),
    ['node_modules/fork-ts-checker-webpack-plugin']);
  for (const [name, pkg] of Object.entries(lock.packages)) {
    assert(!name.endsWith('/deepmerge'), name);
    assert(!Object.hasOwn(pkg.dependencies || {}, 'deepmerge'), name);
  }
  assert.equal(lock.packages['node_modules/@storybook/builder-webpack5'].dependencies['fork-ts-checker-webpack-plugin'], '^8.0.0');
  assert.equal(lock.packages['node_modules/deepmerge-ts'].version, '8.0.2');
  assert.equal(lock.packages['node_modules/deepmerge-ts'].license, 'BSD-3-Clause');
  const docker = readFileSync(path.join(root, '../../sandbox-images/headlamp/Dockerfile'), 'utf8');
  const copy = `COPY tools/headlamp-plugin/checker/${filename} ./checker/`;
  assert(docker.includes(copy));
  assert(docker.indexOf(copy) < docker.indexOf('RUN npm ci'));
  const ignore = readFileSync(path.join(root, '../../sandbox-images/headlamp/Dockerfile.dockerignore'), 'utf8');
  assert(ignore.includes(`!tools/headlamp-plugin/checker/${filename}`));
});

test('explicit options take precedence while preserving nested defaults and cloning arrays', () => {
  const external = { typescript: { compilerOptions: { strict: true, types: ['base'] }, memoryLimit: 1024 }, issue: { include: [{ file: 'a.ts' }] } };
  const explicit = { typescript: { compilerOptions: { strict: false, types: ['plugin'] } }, issue: { include: [{ file: 'b.ts' }] } };
  const result = merge(external, explicit);
  assert.deepEqual(result, { typescript: { compilerOptions: { strict: false, types: ['base', 'plugin'] }, memoryLimit: 1024 }, issue: { include: [{ file: 'a.ts' }, { file: 'b.ts' }] } });
  result.issue.include[0].file = 'changed';
  result.issue.include[1].file = 'changed';
  assert.equal(external.issue.include[0].file, 'a.ts');
  assert.equal(explicit.issue.include[0].file, 'b.ts');
});

test('arrays concatenate with holes preserved and array/object mismatches replace', () => {
  const a = [{ nested: true }, , 3];
  const b = [, { other: true }];
  const result = merge({ a, b: { old: true }, c: [1] }, { a: b, b: [2], c: { new: true } });
  assert.deepEqual(result, { a: [{ nested: true }, , 3, , { other: true }], b: [2], c: { new: true } });
  assert.notEqual(result.a[0], a[0]);
  assert.notEqual(result.a[4], b[1]);
});

test('undefined, null, false, zero and empty strings remain explicit overrides', () => {
  const result = merge({ a: 1, b: 2, c: true, d: 9, e: 'text' }, { a: undefined, b: null, c: false, d: 0, e: '' });
  assert.deepEqual(result, { a: undefined, b: null, c: false, d: 0, e: '' });
  assert(Object.hasOwn(result, 'a'));
});

test('special references survive while class/map/set enumerable properties retain old cloning semantics', () => {
  const date = new Date(0), regexp = /value/g, fn = () => true;
  const element = { $$typeof: Symbol.for('react.element'), props: { child: 'test' } };
  class Options { constructor() { this.own = { nested: true }; } }
  const instance = new Options();
  const result = merge({ element: { discarded: true }, replacement: element }, { date, regexp, fn, element, instance, set: new Set([1]), map: new Map([[1, 2]]), replacement: { value: 1 } });
  for (const [key, value] of Object.entries({ date, regexp, fn, element })) assert.equal(result[key], value);
  assert.deepEqual(result.instance, { own: { nested: true } });
  assert.notEqual(result.instance.own, instance.own);
  assert.deepEqual(result.set, {});
  assert.deepEqual(result.map, {});
  assert.deepEqual(result.replacement, { value: 1 });
  assert.equal(merge({ element }, {}).element, element);
  assert.equal(merge({}, { elements: [element] }).elements[0], element);
});

test('only own enumerable string and symbol keys are copied, never inherited or hidden keys', () => {
  const symbol = Symbol('option');
  const external = Object.create({ inherited: 'ignored' });
  external.own = { first: true };
  external[symbol] = { first: true };
  Object.defineProperty(external, 'hidden', { value: 'ignored' });
  const explicit = Object.create(null);
  explicit.own = { second: true };
  explicit[symbol] = { second: true };
  const result = merge(external, explicit);
  assert.deepEqual(result, { own: { first: true, second: true }, [symbol]: { first: true, second: true } });
  assert.equal(Object.getPrototypeOf(result), Object.prototype);
});

for (const side of ['external', 'explicit']) {
  test(`prototype poison in ${side} options stays inert at root, nested and array positions`, () => {
    const payload = JSON.parse('{"__proto__":{"checkerPoisoned":true},"constructor":{"prototype":{"checkerPoisoned":true}},"toString":"own-data"}');
    const source = { ...payload, nested: payload, array: [payload] };
    const result = side === 'external' ? merge(source, {}) : merge({}, source);
    for (const item of [result, result.nested, result.array[0]]) {
      assert.equal(Object.getPrototypeOf(item), Object.prototype);
      assert.equal(item.checkerPoisoned, undefined);
      assert.equal(Object.getOwnPropertyDescriptor(item, '__proto__').value.checkerPoisoned, true);
      assert.deepEqual(item.constructor, { prototype: { checkerPoisoned: true } });
      assert.equal(item.toString, 'own-data');
    }
    assert.equal({}.checkerPoisoned, undefined);
    assert.equal(Object.getPrototypeOf(payload), Object.prototype);
    assert.notEqual(result.nested.__proto__, payload.__proto__);
    assert.notEqual(result.array[0].constructor, payload.constructor);
  });
}

test('colliding poison keys merge as own data without changing either input or the output prototype', () => {
  const external = JSON.parse('{"nested":{"__proto__":{"external":true},"constructor":{"prototype":{"external":true}}}}');
  const explicit = JSON.parse('{"nested":{"__proto__":{"explicit":true},"constructor":{"prototype":{"explicit":true}}}}');
  const result = merge(external, explicit);
  assert.equal(Object.getPrototypeOf(result.nested), Object.prototype);
  assert.deepEqual(Object.getOwnPropertyDescriptor(result.nested, '__proto__').value, { external: true, explicit: true });
  assert.deepEqual(result.nested.constructor.prototype, { external: true, explicit: true });
  assert.equal(result.nested.external, undefined);
  assert.equal(result.nested.explicit, undefined);
  assert.equal({}.external, undefined);
  assert.equal({}.explicit, undefined);
  assert.deepEqual(external.nested.__proto__, { external: true });
  assert.deepEqual(explicit.nested.__proto__, { explicit: true });
});

test('real checker constructor merges cosmiconfig and explicit options and still rejects invalid options', () => {
  writeFileSync(path.join(fixture, '.fork-ts-checkerrc.json'), JSON.stringify({
    typescript: { compilerOptions: { strict: true, types: ['external'] }, memoryLimit: 768 },
    issue: { include: [{ file: 'external.ts' }] },
  }));
  execFileSync(process.execPath, ['-e', `
    const assert = require('node:assert/strict');
    const Checker = require(${JSON.stringify(checker)});
    const plugin = new Checker({ typescript: { compilerOptions: { strict: false, types: ['explicit'] } }, issue: { include: [{ file: 'explicit.ts' }] } });
    assert.deepEqual(plugin.options.typescript, { compilerOptions: { strict: false, types: ['external', 'explicit'] }, memoryLimit: 768 });
    assert.deepEqual(plugin.options.issue.include, [{ file: 'external.ts' }, { file: 'explicit.ts' }]);
    assert.equal(typeof plugin.apply, 'function');
    assert.equal(typeof Checker.getCompilerHooks, 'function');
    assert.throws(() => new Checker({ typescript: { memoryLimit: 'invalid' } }), /memoryLimit should be a number/);
  `], { cwd: fixture, stdio: 'pipe' });
  writeFileSync(path.join(fixture, '.fork-ts-checkerrc.json'), JSON.stringify({ typescript: { memoryLimit: 'invalid' } }));
  execFileSync(process.execPath, ['-e', `
    const assert = require('node:assert/strict');
    const Checker = require(${JSON.stringify(checker)});
    assert.throws(() => new Checker(), /memoryLimit should be a number/);
  `], { cwd: fixture, stdio: 'pipe' });
});
