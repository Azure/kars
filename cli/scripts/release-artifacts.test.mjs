// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { cpSync, mkdirSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse } from 'yaml';
import { bindImages, bridgeImages, coreImages, prepareCharts, readReceipts, releaseVersion, requiredImages } from './release-artifacts.mjs';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const digest = `sha256:${'a'.repeat(64)}`;
function images() {
  return Object.fromEntries(requiredImages.map(name => [name, {
    name, repository: name === 'seccomp-installer' ? 'mcr.microsoft.com/azurelinux/base/core' : `ghcr.io/azure/${name}`, digest,
  }]));
}
function fixture(t) {
  const root = join(repo, 'cli', `.release-packaging-test-${randomUUID()}`);
  mkdirSync(root);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}
function run(root, command, args) {
  const result = spawnSync(command, args, {
    cwd: root, encoding: 'utf8', timeout: 60000,
    env: { ...process.env, TMPDIR: root, npm_config_cache: join(root, '.npm-cache') },
  });
  assert.equal(result.status, 0, `${command} ${args.join(' ')}\n${result.stderr}\n${result.stdout}`);
  return result.stdout;
}

test('SemVer selects beta channels without accidentally treating every release as stable', () => {
  for (const input of ['v1.2.3-beta.1', '1.2.3-rc.2', 'v0.1.27-interim.1', '1.2.3-0']) {
    assert.equal(releaseVersion(input).prerelease, true);
    assert.equal(releaseVersion(input).npmTag, 'next');
  }
  assert.deepEqual(releaseVersion('1.2.3+build.5'), {
    version: '1.2.3+build.5', tag: 'v1.2.3+build.5', imageTag: 'v1.2.3_build.5', prerelease: false, npmTag: 'latest',
  });
  for (const input of ['', 'kars-bridge', 'interim-sha', 'v01.2.3', 'v1.2.3-beta.01', 'v1.2.3\n', 'v1.2.3;echo bad', 'v1.2.3-']) {
    assert.throws(() => releaseVersion(input), /SemVer/);
  }
});

test('image binding preserves profile policy and image field conventions', () => {
  const values = parse(readFileSync(join(repo, 'deploy/helm/kars/values.yaml'), 'utf8'));
  const original = structuredClone(values);
  bindImages(values, coreImages, images());
  assert.equal(values.schemaHook.image.tag, 'latest');
  assert.equal(values.schemaHook.image.digest, digest);
  assert.equal(values.controller.image.tag, `latest@${digest}`);
  assert.equal(values.monitoring.prometheusOperator.image.registry, 'ghcr.io');
  assert.equal(values.monitoring.prometheusOperator.image.repository, 'azure/kars-prometheus-operator');
  assert.equal(values.runtimes.hermes.image, `ghcr.io/azure/kars-runtime-hermes:latest@${digest}`);
  for (const path of ['networkPolicy', 'azure', 'sre', 'auth', 'signerPolicy']) assert.deepEqual(values[path], original[path]);
  assert.equal(values.inferenceBudget.calendarPolicy, 'enforce');
  assert.equal(values.schemaHook.enabled, false);
  const beta = parse(readFileSync(join(repo, 'deploy/helm/kars/values-kind-beta.yaml'), 'utf8'));
  assert.deepEqual(bindImages(structuredClone(beta), coreImages, images(), true), beta);
  assert.equal(beta.inferenceBudget.calendarPolicy, 'record-only');
  assert.deepEqual(beta.sre.firstInstallFunding, { unboundedTokens: true });
});

test('Bridge BFF Docker context includes the compiled evaluation catalog without broadening exclusions', () => {
  const source = 'eval-corpus/src/builtin_names.rs';
  const dockerfile = readFileSync(join(repo, 'bridge/bff/Dockerfile'), 'utf8');
  assert.ok(dockerfile.split('\n').some(line => line.startsWith(`COPY ${source} `)));
  assert.ok(readFileSync(join(repo, source)).length > 0);
  const rules = readFileSync(join(repo, 'bridge/bff/Dockerfile.dockerignore'), 'utf8')
    .split('\n').map(line => line.trim()).filter(line => line && !line.startsWith('#'));
  assert.equal(rules[0], '**');
  for (const rule of ['!eval-corpus/', '!eval-corpus/src/', `!${source}`]) assert.ok(rules.includes(rule), `Missing context allowlist: ${rule}`);
  assert.ok(!rules.includes('!eval-corpus/**'));
  const lastInclude = Math.max(...rules.map((rule, index) => rule.startsWith('!') ? index : -1));
  for (const rule of ['**/target', '**/node_modules', '**/.git', '**/.env*', '**/*.key', '**/*.pfx', '**/*.p12', '**/*.kubeconfig']) {
    assert.ok(rules.indexOf(rule) > lastInclude, `Context exclusion must take precedence: ${rule}`);
  }
});

test('missing, duplicate, untrusted and malformed receipts fail closed', t => {
  const root = fixture(t);
  assert.throws(() => readReceipts(root), /Missing/);
  for (const receipt of Object.values(images())) writeFileSync(join(root, `${receipt.name}.json`), JSON.stringify(receipt));
  assert.equal(Object.keys(readReceipts(root)).length, requiredImages.length);
  writeFileSync(join(root, 'duplicate.json'), JSON.stringify(images()['kars-controller']));
  assert.throws(() => readReceipts(root), /duplicate/);
  rmSync(join(root, 'duplicate.json'));
  writeFileSync(join(root, 'kars-controller.json'), JSON.stringify({ ...images()['kars-controller'], repository: 'untrusted/controller' }));
  assert.throws(() => readReceipts(root), /Invalid/);
  writeFileSync(join(root, 'kars-controller.json'), JSON.stringify({ ...images()['kars-controller'], digest: 'latest' }));
  assert.throws(() => readReceipts(root), /Invalid/);
});

for (const version of ['0.0.0-test.1', '0.0.0-test.1+build.5']) test(`clean packaging retains locked dependencies and exact version ${version}`, async t => {
  const root = fixture(t);
  for (const relative of ['deploy/helm/kars', 'deploy/helm/kars-datapath-witness', 'bridge/deploy/helm/kars-bridge']) {
    cpSync(join(repo, relative), join(root, relative), { recursive: true });
  }
  mkdirSync(join(root, 'receipts'));
  for (const receipt of Object.values(images())) writeFileSync(join(root, 'receipts', `${receipt.name}.json`), JSON.stringify(receipt));
  await prepareCharts(root, join(root, 'packages'), version, join(root, 'receipts'));
  for (const [relative, name] of [['deploy/helm/kars', 'kars'], ['bridge/deploy/helm/kars-bridge', 'kars-bridge']]) {
    run(root, 'helm', ['package', relative, '--destination', 'packages']);
    const metadata = parse(run(root, 'helm', ['show', 'chart', `packages/${name}-${version}.tgz`]));
    assert.equal(metadata.version, version);
    assert.equal(metadata.appVersion, version);
  }
  const core = parse(run(root, 'helm', ['show', 'values', `packages/kars-${version}.tgz`]));
  assert.equal(core.controller.image.tag, `latest@${digest}`);
  const rendered = run(root, 'helm', ['template', 'kars', `packages/kars-${version}.tgz`, '--namespace', 'kars-system', '--kube-version', '1.35.0', '--values', 'deploy/helm/kars/values-generic.yaml']);
  for (const match of rendered.matchAll(/\bimage:[ \t]+["']?([^\s"']+)/g)) {
    assert.ok(match[1].endsWith(`@${digest}`), `Unbound rendered image: ${match[1]}`);
  }
  const bridge = parse(run(root, 'helm', ['show', 'values', `packages/kars-bridge-${version}.tgz`]));
  assert.equal(bridge.bff.image.repository, 'ghcr.io/azure/kars-bridge-bff');
  assert.equal(bridge.idp.dex.image, `ghcr.io/azure/kars-bridge-idp:latest@${digest}`);
  const renderedBridge = run(root, 'helm', ['template', 'kars-bridge', `packages/kars-bridge-${version}.tgz`, '--namespace', 'kars-system', '--kube-version', '1.35.0']);
  for (const match of renderedBridge.matchAll(/\bimage:[ \t]+["']?([^\s"']+)/g)) assert.ok(match[1].endsWith(`@${digest}`), match[1]);
  const lock = parse(readFileSync(join(root, 'deploy/helm/kars/Chart.lock'), 'utf8'));
  assert.equal(readFileSync(join(root, 'deploy/helm/kars/Chart.lock'), 'utf8'), readFileSync(join(repo, 'deploy/helm/kars/Chart.lock'), 'utf8'));
  const chartEntries = run(root, 'tar', ['-tzf', `packages/kars-${version}.tgz`]);
  for (const dependency of lock.dependencies) assert.ok(chartEntries.includes(`kars/charts/${dependency.name}/Chart.yaml`), chartEntries);
  mkdirSync(join(root, 'cli/scripts'), { recursive: true });
  cpSync(join(repo, 'cli/scripts/bundle-deploy-assets.mjs'), join(root, 'cli/scripts/bundle-deploy-assets.mjs'));
  cpSync(join(repo, 'cli/package.json'), join(root, 'cli/package.json'));
  cpSync(join(repo, 'cli/package-lock.json'), join(root, 'cli/package-lock.json'));
  cpSync(join(repo, 'deploy/agentmesh-agt.yaml'), join(root, 'deploy/agentmesh-agt.yaml'));
  symlinkSync(join(repo, 'cli/node_modules'), join(root, 'cli/node_modules'), 'dir');
  run(root, process.execPath, [join(repo, 'cli/scripts/release-artifacts.mjs'), 'cli-version', join(root, 'cli'), version]);
  run(root, process.execPath, ['cli/scripts/bundle-deploy-assets.mjs']);
  run(join(root, 'cli'), 'npm', ['pack', '--ignore-scripts', '--pack-destination', '../packages']);
  const entries = run(root, 'tar', ['-tzf', `packages/kars-runtime-cli-${version}.tgz`]);
  for (const dep of lock.dependencies) assert.ok(entries.includes(`package/dist/deploy/helm/kars/charts/${dep.name}-${dep.version}.tgz`));
  assert.ok(entries.includes('package/dist/bridge/deploy/helm/kars-bridge/Chart.yaml'));
  assert.equal(JSON.parse(run(root, 'tar', ['-xOf', `packages/kars-runtime-cli-${version}.tgz`, 'package/package.json'])).version, version);
  assert.equal(parse(run(root, 'tar', ['-xOf', `packages/kars-runtime-cli-${version}.tgz`, 'package/dist/deploy/helm/kars/Chart.yaml'])).version, version);
  // No source defaults are rewritten by preparing the isolated packaging checkout.
  assert.notEqual(parse(readFileSync(join(repo, 'deploy/helm/kars/values.yaml'), 'utf8')).controller.image.tag, `latest@${digest}`);
});

test('workflow publishes the complete chart image inventory through the existing pipeline', () => {
  const workflow = parse(readFileSync(join(repo, '.github/workflows/release-public-interim.yml'), 'utf8'));
  const jobs = workflow.jobs;
  const built = new Set(jobs['build-images'].strategy.matrix.image.map(image => image.ghcr));
  for (const runtime of jobs['build-runtimes'].strategy.matrix.runtime) built.add(runtime.ghcr);
  for (const name of ['openclaw-sandbox', 'kars-sandbox-base', 'kars-agentmesh-registry', 'kars-agentmesh-relay', 'seccomp-installer']) built.add(name);
  assert.deepEqual([...built].sort(), [...requiredImages].sort());
  for (const image of jobs['build-images'].strategy.matrix.image) {
    assert.ok(readFileSync(join(repo, image.dockerfile)).length > 0);
    assert.ok(readdirSync(join(repo, image.context || '.')).length > 0);
  }
  const pack = jobs['cli-pack'].steps;
  const index = text => pack.findIndex(step => step.run?.includes(text));
  assert.ok(index('helm dependency build deploy/helm/kars') < index('npm run build'));
  assert.ok(index('release-artifacts.mjs prepare') < index('npm run build'));
  assert.ok(pack.some(step => step.run?.includes('release-artifacts.mjs cli-version')));
  assert.ok(pack.some(step => step.run?.includes('--tag "$NPM_CHANNEL"')));
  for (const job of ['build-images', 'build-runtimes']) {
    const tags = jobs[job].steps.find(step => step.with?.tags)?.with.tags;
    assert.match(tags, /prerelease == 'false'/);
  }
  for (const job of ['merge-sandbox', 'merge-mesh']) assert.ok(jobs[job].steps.some(step => step.run?.includes('if [ "$PRERELEASE" = false ]')));
  const release = jobs.release.steps.find(step => step.uses?.startsWith('softprops/action-gh-release'));
  assert.match(release.with.prerelease, /needs.prepare.outputs.prerelease/);
  assert.match(release.with.make_latest, /'false'/);
  assert.equal(jobs['cli-pack'].permissions['id-token'], 'write');
  assert.equal(Object.keys(bridgeImages).length, 4);
  for (const [name, job] of Object.entries(jobs)) {
    if (name !== 'prepare') assert.ok([job.needs].flat().includes('prepare'), `${name} must use the validated version`);
    for (const step of job.steps) {
      if (!step.run) continue;
      const script = step.run.replace(/\$\{\{[\s\S]*?\}\}/g, 'validated-expression');
      const checked = spawnSync('bash', ['-n'], { input: script, encoding: 'utf8' });
      assert.equal(checked.status, 0, `${name}/${step.name}: ${checked.stderr}`);
    }
  }
});
