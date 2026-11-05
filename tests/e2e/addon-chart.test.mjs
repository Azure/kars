// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import test from 'node:test';
import { targets, values } from './addon-images.mjs';

const { parseAllDocuments } = createRequire(new URL('../../cli/package.json', import.meta.url))('yaml');
const render = overrides => parseAllDocuments(execFileSync('helm', [
  'template', 'kars', 'deploy/helm/kars', '--namespace', 'kars-system', '--values', '-',
], { input: JSON.stringify(overrides), encoding: 'utf8' })).map(document => {
  assert.equal(document.errors.length, 0);
  return document.toJSON();
}).filter(Boolean);
const production = render({});
const fixture = render(values());
const images = documents => {
  const result = [];
  const visit = node => {
    if (!node || typeof node !== 'object') return;
    if (typeof node.image === 'string' && node.image.startsWith('ghcr.io/azure/kars-')) {
      result.push({ image: node.image, policy: node.imagePullPolicy });
    }
    Object.values(node).forEach(visit);
  };
  documents.forEach(visit);
  return result;
};

test('actual pinned chart uses prepared images, including both certificate hooks', () => {
  const prepared = new Set(targets.map(target => `ghcr.io/azure/${target.repo}:latest`));
  const rendered = images(fixture);
  assert.equal(rendered.length, 9);
  for (const item of rendered) {
    assert.ok(prepared.has(item.image), `Unprepared chart add-on: ${item.image}`);
    assert.equal(item.policy, 'Never', item.image);
  }
  const reloader = 'ghcr.io/azure/kars-prometheus-config-reloader:latest';
  const operator = fixture.find(document => document.kind === 'Deployment'
    && document.metadata.name === 'kps-kube-prometheus-stack-operator');
  assert.ok(operator.spec.template.spec.containers[0].args.includes(`--prometheus-config-reloader=${reloader}`));
  assert.ok(prepared.has(reloader));
  assert.deepEqual(new Set([...rendered.map(item => item.image), reloader]), prepared);
  const hooks = fixture.filter(document => document.kind === 'Job');
  assert.equal(hooks.filter(document => document.metadata.annotations['helm.sh/hook'].includes('pre-upgrade')).length, 1);
});

test('fixture overrides preserve enabled workloads, image references and production Always policies', () => {
  assert.deepEqual(fixture.map(document => [document.kind, document.metadata.name]),
    production.map(document => [document.kind, document.metadata.name]));
  assert.deepEqual(images(fixture).map(item => item.image), images(production).map(item => item.image));
  assert.ok(images(production).every(item => item.policy === 'Always'));
});
