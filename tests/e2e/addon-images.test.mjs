// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { build, load, policies, targets, values, verify } from './addon-images.mjs';

const id = `sha256:${'a'.repeat(64)}`;
const inventory = () => targets.map(target => ({ image: `ghcr.io/azure/${target.repo}:latest`, id }));
const recorder = () => {
  const calls = [];
  return { calls, execute: (...args) => { calls.push(args); return `${id}\n`; } };
};

test('builds all nine enabled add-ons using the existing target recipes', () => {
  const { calls, execute } = recorder();
  assert.deepEqual(build(execute), inventory());
  const builds = calls.filter(([, args]) => args[0] === 'build');
  assert.equal(builds.length, 9);
  for (const [index, target] of targets.entries()) {
    assert.deepEqual(builds[index], ['docker', ['build', '--tag', inventory()[index].image,
      '--file', target.dockerfile, ...(target.target ? ['--target', target.target] : []), '.']]);
  }
  assert.deepEqual(targets.filter(target => target.target).map(target => target.target), ['operator', 'config-reloader']);
});

test('reuses only explicitly provided Headlamp and still builds the other eight', () => {
  const { calls, execute } = recorder();
  assert.deepEqual(build(execute, true), inventory());
  assert.equal(calls.filter(([, args]) => args[0] === 'build').length, 8);
  assert.deepEqual(calls.find(([, args]) => args[0] === 'tag'),
    ['docker', ['tag', 'kars-headlamp:latest', 'ghcr.io/azure/kars-headlamp:latest']]);
});

test('missing qualified Headlamp fails rather than silently rebuilding or pulling', () => {
  const { calls, execute } = recorder();
  assert.throws(() => build((file, args, capture) => {
    if (args.includes('kars-headlamp:latest')) throw new Error('missing');
    return execute(file, args, capture);
  }, true), /missing/);
  assert.ok(!calls.some(([, args]) => args[0] === 'pull' || args[0] === 'tag'));
});

test('rejects missing, duplicate, reordered, foreign and malformed inventories before loading', () => {
  const good = inventory();
  for (const bad of [undefined, [], good.slice(1), [...good, good[0]],
    [good[1], ...good.slice(1)], [...good].reverse(),
    [{ ...good[0], image: 'foreign:latest' }, ...good.slice(1)],
    [{ ...good[0], id: 'unbound' }, ...good.slice(1)]]) {
    const { calls, execute } = recorder();
    assert.throws(() => load(bad, execute), /inventory/);
    assert.equal(calls.length, 0);
  }
});

test('changed and absent local images fail before any Kind mutation or registry pull', () => {
  for (const answer of [`sha256:${'b'.repeat(64)}`, 'invalid', null]) {
    const calls = [];
    assert.throws(() => load(inventory(), (file, args) => {
      calls.push([file, args]);
      if (answer === null) throw new Error('missing image');
      return answer;
    }));
    assert.ok(calls.every(([file, args]) => file === 'docker' && args[0] === 'image' && args[1] === 'inspect'));
  }
});

test('verifies every identity before loading only the dedicated disposable cluster', () => {
  const { calls, execute } = recorder();
  load(inventory(), execute);
  assert.equal(calls.length, 10);
  assert.ok(calls.slice(0, 9).every(([file, args]) => file === 'docker' && args[0] === 'image'));
  assert.deepEqual(calls[9], ['kind', ['load', 'docker-image', ...inventory().map(item => item.image), '--name', 'kars-e2e']]);
  assert.doesNotThrow(() => verify(inventory(), execute));
});

test('fixture overrides only pull policies, preserving image names and all enabled features', () => {
  const flattened = [];
  function visit(node, path = []) {
    for (const [key, value] of Object.entries(node)) {
      if (typeof value === 'object') visit(value, [...path, key]);
      else { assert.equal(value, 'Never'); flattened.push([...path, key].join('.')); }
    }
  }
  visit(values());
  assert.deepEqual(flattened.sort(), [...policies].sort());
  assert.equal(flattened.length, 8);
  assert.ok(flattened.includes('monitoring.prometheusOperator.admissionWebhooks.patch.image.pullPolicy'));
});

test('historical install retains fixture values into authority stage and final install', () => {
  const fixture = readFileSync(new URL('./sre_authority/fixtures.py', import.meta.url), 'utf8');
  const runner = readFileSync(new URL('./run.sh', import.meta.url), 'utf8');
  assert.ok(fixture.includes('"--values", str(h.root / ".e2e-addon-values.json")'));
  assert.ok(runner.includes('--values "$ROOT_DIR/.e2e-addon-values.json"'));
  assert.match(runner, /build_images\s+prepare_addon_images\s+prepare_managed_mcp\s+prepare_sre_authority_legacy/);
  assert.match(fixture, /"--router-image", "kars-inference-router:e2e", timeout=180/);
});

test('hosted images stay same-run, qualified, unpublished and fail closed', () => {
  const workflow = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');
  assert.ok(workflow.includes('needs: [changes, headlamp-plugin-build]'));
  assert.ok(workflow.includes('needs: [changes, build-rust, kind-addon-images]'));
  for (const name of ['kind-headlamp', 'kind-addons']) {
    assert.equal(workflow.split(`name: ${name}-\${{ github.sha }}-\${{ github.run_attempt }}`).length - 1, 2);
  }
  assert.ok(workflow.indexOf('Export qualified Headlamp') > workflow.indexOf('Scan canonical Headlamp'));
  assert.ok(workflow.includes('KARS_E2E_PREBUILT_ADDONS: "1"'));
  const helper = readFileSync(new URL('./addon-images.mjs', import.meta.url), 'utf8');
  assert.ok(helper.includes('receipt.revision !== revision'));
  assert.ok(!helper.includes("'pull'"));
  assert.ok(!helper.includes("'push'"));
});
