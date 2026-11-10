// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdirSync, readFileSync, rmSync } from 'node:fs';
import path from 'node:path';
import { after, before, test } from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { x } from 'tar';
import { parseAllDocuments } from 'yaml';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.dirname(here);
const read = file => readFileSync(file, 'utf8');
const json = file => JSON.parse(read(file));
const hash = file => createHash('sha256').update(readFileSync(file)).digest('hex');
const provenance = json(path.join(here, 'upstream.json'));
const manifest = json(path.join(root, 'package.json'));
const lock = json(path.join(root, 'package-lock.json'));
const filename = 'headlamp-plugin-0.13.1-kars.tgz';
const fixture = path.join(root, `.polyfill-test-${randomUUID()}`);
const tool = path.join(fixture, 'node_modules/@kinvolk/headlamp-plugin');
const toolLock = lock.packages['node_modules/@kinvolk/headlamp-plugin'];
let packaged;

before(() => {
  const archive = path.join(here, filename);
  const [algorithm, expected] = toolLock.integrity.split('-');
  assert.equal(createHash(algorithm).update(readFileSync(archive)).digest('base64'), expected);
  mkdirSync(tool, { recursive: true });
  x({ file: archive, cwd: tool, strip: 1, sync: true });
  packaged = json(path.join(tool, 'package.json'));
});
after(() => rmSync(fixture, { recursive: true, force: true }));

test('tool archive preserves upstream identity, attribution, CLI and Storybook consumers', () => {
  assert.equal(packaged.name, provenance.name);
  assert.equal(packaged.version, provenance.version);
  assert.equal(packaged.license, 'Apache 2.0');
  assert.deepEqual(packaged.karsPatch.upstream, provenance);
  assert.deepEqual(json(path.join(tool, 'KARS-PATCH.json')), packaged.karsPatch);
  assert.equal(hash(path.join(tool, 'UPSTREAM-LICENSE')), provenance.licenseSha256);
  assert.equal(read(path.join(tool, 'KARS-LICENSE')), read(path.join(root, 'checker/LICENSE')));
  for (const name of ['bin/headlamp-plugin.js', 'config/.storybook/main.js']) {
    assert.equal(hash(path.join(tool, name)), provenance.sourceHashes[name], name);
  }
  assert.equal(packaged.dependencies['@storybook/react-webpack5'], '^9.1.17');
  assert.equal(packaged.dependencies.typescript, '5.6.2');
  assert.equal(packaged.dependencies['ts-loader'], '^9.5.2');
});

test('one installed tool path removes the actual unused crypto graph without removing the checker', () => {
  const spec = `file:build-tool/${filename}`;
  assert.equal(manifest.devDependencies['@kinvolk/headlamp-plugin'], spec);
  assert.equal(lock.packages[''].devDependencies['@kinvolk/headlamp-plugin'], spec);
  assert.equal(toolLock.resolved, spec);
  assert.deepEqual(toolLock.dependencies, packaged.dependencies);
  assert.deepEqual(Object.keys(lock.packages).filter(key => key.endsWith('/@kinvolk/headlamp-plugin')),
    ['node_modules/@kinvolk/headlamp-plugin']);
  for (const [name, version] of Object.entries(provenance.replacements)) {
    assert.equal(packaged.dependencies[name], version);
    assert.equal(lock.packages[`node_modules/${name}`].version, version);
  }
  for (const [key, dependency] of Object.entries(lock.packages)) {
    for (const removed of ['elliptic', 'crypto-browserify', 'node-stdlib-browser', 'vite-plugin-node-polyfills', 'deepmerge']) {
      assert(!key.endsWith(`/${removed}`), key);
      assert(!Object.hasOwn(dependency.dependencies ?? {}, removed), `${key} still requests ${removed}`);
    }
  }
  assert.equal(lock.packages['node_modules/@storybook/builder-webpack5'].dependencies['fork-ts-checker-webpack-plugin'], '^8.0.0');
  assert.equal(lock.packages['node_modules/fork-ts-checker-webpack-plugin'].resolved,
    manifest.devDependencies['fork-ts-checker-webpack-plugin']);
});

test('packaged helper is generated from the single maintained frontend polyfill implementation', () => {
  const canonical = read(path.join(root, '../../sandbox-images/headlamp/build-frontend.mjs'));
  const implementation = canonical.slice(canonical.indexOf('export function productionPolyfills('),
    canonical.indexOf('\nexport function verifyProductionLock(')).trim();
  const helperPath = path.join(tool, 'config/kars-polyfills.mjs');
  assert.equal(hash(helperPath), packaged.karsPatch.polyfillsSha256);
  const helper = read(helperPath);
  assert(helper.includes(implementation));
  assert.equal(helper.split('export function productionPolyfills(').length, 2);
  assert(helper.endsWith('export default productionPolyfills(require("@rollup/plugin-inject"), require.resolve);\n'));
  const config = read(path.join(tool, 'config/vite.config.mjs'));
  assert(config.includes("import polyfills from './kars-polyfills.mjs';"));
  assert(config.includes('    polyfills,'));
  assert(!config.includes('vite-plugin-node-polyfills'));
  assert(!config.includes('fileURLToPath'));
});

test('real packaged Vite configuration loads the same build/watch polyfills and preserves UMD externals', async () => {
  const { default: config } = await import(pathToFileURL(path.join(tool, 'config/vite.config.mjs')));
  const plugin = config.plugins.flat().find(p => p.name === 'kars-headlamp-production-polyfills');
  assert(plugin);
  assert.equal(plugin.apply, 'build');
  const aliases = Object.fromEntries(plugin.config().resolve.alias.map(({ find, replacement }) => [find, replacement]));
  assert.deepEqual(Object.keys(aliases).sort(), ['buffer', 'node:buffer', 'node:process', 'node:stream', 'process', 'stream']);
  for (const name of ['buffer', 'process', 'stream']) assert.equal(aliases[name], aliases[`node:${name}`]);
  for (const name of ['crypto', 'node:crypto']) assert.throws(() => plugin.resolveId(name), /real reviewed implementation/);
  assert.equal(plugin.load('crypto'), null);
  assert.equal(config.define.global, 'globalThis');
  assert.deepEqual(config.build.lib.formats, ['umd']);
  assert.equal(config.build.rollupOptions.output.globals('@mui/material'), 'pluginLib.MuiMaterial');
  assert.equal(config.build.rollupOptions.output.globals('@mui/material/Grid'), 'pluginLib.MuiMaterial.Grid');
  assert(config.build.rollupOptions.external.includes('react'));
});

test('image installs the patched tool archive before npm ci and still applies the real example repair', () => {
  const docker = read(path.join(root, '../../sandbox-images/headlamp/Dockerfile'));
  const copy = `COPY tools/headlamp-plugin/build-tool/${filename} ./build-tool/`;
  assert(docker.includes(copy));
  assert(docker.indexOf(copy) < docker.indexOf('RUN npm ci'));
  assert(read(path.join(root, '../../sandbox-images/headlamp/Dockerfile.dockerignore'))
    .includes(`!tools/headlamp-plugin/build-tool/${filename}`));
  assert.equal(manifest.scripts.postinstall, 'node patch-build-tool.mjs');
});

test('KEDA example keeps real authentication consumers but requires externally provisioned Secrets', () => {
  const contract = packaged.karsPatch.exampleAuthentication;
  const authPath = path.join(tool, contract.path);
  assert.equal(hash(authPath), contract.sha256);
  const documents = parseAllDocuments(read(authPath));
  for (const document of documents) assert.deepEqual(document.errors, []);
  assert.deepEqual(documents.map(document => document.toJS()), [
    {
      apiVersion: 'keda.sh/v1alpha1', kind: 'TriggerAuthentication',
      metadata: { name: 'rabbitmq-trigger-auth', namespace: 'default' },
      spec: { secretTargetRef: [{ parameter: 'host', name: 'queue-connection-secrets', key: 'RABBITMQ_CONNECTION_STRING' }] },
    },
    {
      apiVersion: 'keda.sh/v1alpha1', kind: 'TriggerAuthentication',
      metadata: { name: 'redis-trigger-auth', namespace: 'default' },
      spec: { secretTargetRef: [
        { parameter: 'username', name: 'redis-scaler-secret', key: 'redis_username' },
        { parameter: 'password', name: 'redis-scaler-secret', key: 'redis_password' },
      ] },
    },
  ]);
  for (const file of ['consumer-deployment.yaml', 'publisher-job.yaml', 'scaledJob.yaml', 'scaledObject.yaml']) {
    const source = read(path.join(path.dirname(authPath), file));
    for (const document of parseAllDocuments(source)) {
      assert.deepEqual(document.errors, []);
      assert.notEqual(document.toJS()?.kind, 'Secret');
    }
    if (file === 'scaledObject.yaml' || file === 'scaledJob.yaml') {
      assert(source.includes('name: rabbitmq-trigger-auth'));
      assert(source.includes('name: redis-trigger-auth'));
    }
    if (file !== 'scaledObject.yaml') {
      assert(source.includes('name: queue-connection-secrets'));
      assert(source.includes('key: RABBITMQ_CONNECTION_STRING'));
      assert(source.includes('key: REDIS_CONNECTION_STRING'));
    }
  }
});

test('packaged credential instructions provision matching Secrets from private files without embedded values', () => {
  const contract = packaged.karsPatch.exampleAuthentication;
  const guidePath = path.join(tool, contract.credentialsGuide);
  assert.equal(hash(guidePath), contract.credentialsGuideSha256);
  assert.equal(read(guidePath), read(path.join(here, 'keda-example-credentials.md')));
  const guide = read(guidePath);
  for (const name of ['queue-connection-secrets', 'redis-scaler-secret']) {
    assert(guide.includes(`kubectl --namespace default create secret generic ${name}`));
  }
  for (const key of ['RABBITMQ_CONNECTION_STRING', 'REDIS_CONNECTION_STRING', 'redis_username', 'redis_password']) {
    assert(guide.includes(`\`${key}\``));
  }
  assert(guide.includes('--from-env-file="$QUEUE_SECRET_ENV_FILE"'));
  assert(guide.includes('--from-env-file="$REDIS_SECRET_ENV_FILE"'));
  assert(guide.includes('kubectl --namespace default apply -f authentication.yaml'));
  assert(!guide.includes('--from-literal'));
  assert(!/\b(?:amqp|redis)s?:\/\//.test(guide));
});

test('backend uses upstream maintained OpenPGP migration, rejects old imports and tests provenance', () => {
  const script = read(path.join(root, '../../sandbox-images/headlamp/build-backend.sh'));
  assert(script.includes('-require=helm.sh/helm/v3@v3.21.4'));
  assert(script.includes('go list -mod=mod -deps ./cmd/ > "$build/headlamp.deps.txt"'));
  assert(script.includes("grep -Eq '^golang.org/x/crypto/openpgp(/|$)'"));
  assert(script.includes('exit 1\nfi\ngo build'));
  assert(script.includes('./pkg/helm ./pkg/telemetry helm.sh/helm/v3/pkg/provenance'));
  assert(!script.includes('-replace='));
});
