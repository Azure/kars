// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { cpSync, mkdirSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runInNewContext } from 'node:vm';
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
  assert.equal(values.managedMcp.playwrightImage, `ghcr.io/azure/kars-mcp-playwright:latest@${digest}`);
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

test('native-required release images use native runners without duplicate receipt or SBOM names', () => {
  const jobs = parse(readFileSync(join(repo, '.github/workflows/release-public-interim.yml'), 'utf8')).jobs;
  const build = jobs['build-images'];
  const native = jobs['merge-native-images'].strategy.matrix.image;
  assert.deepEqual(native, ['kars-bridge-bff', 'kars-prometheus', 'kars-grafana', 'kars-mcp-playwright']);
  assert.equal(build['timeout-minutes'], 90);
  assert.equal(build['runs-on'], "${{ matrix.image.runner || 'ubuntu-22.04' }}");
  assert.equal(build.env.ARCH_SUFFIX, "${{ matrix.image.arch && format('-{0}', matrix.image.arch) || '' }}");
  const identities = new Set();
  for (const image of build.strategy.matrix.image) {
    const identity = image.ghcr + (image.arch ? `-${image.arch}` : '');
    assert.ok(!identities.has(identity), identity);
    identities.add(identity);
    if (!native.includes(image.ghcr)) {
      assert.equal(image.arch, undefined);
      assert.equal(image.runner, undefined);
      continue;
    }
    assert.equal(image.platforms, `linux/${image.arch}`);
    assert.equal(image.runner || 'ubuntu-22.04', image.arch === 'arm64' ? 'ubuntu-24.04-arm' : 'ubuntu-22.04');
    assert.equal(image.no_binaries, true);
  }
  for (const name of native) {
    assert.deepEqual(build.strategy.matrix.image.filter(image => image.ghcr === name).map(image => image.arch).sort(), ['amd64', 'arm64']);
  }
  assert.match(build.steps.find(step => step.uses?.startsWith('docker/setup-qemu-action')).if, /!matrix.image.arch/);
  const push = build.steps.find(step => step.id === 'push');
  assert.match(push.with.tags, /env.ARCH_SUFFIX/);
  assert.equal(push.with.sbom, true);
  assert.equal(push.with.provenance, true);
  for (const step of build.steps.filter(step => step.name === 'Record image build receipt' || step.with?.name?.startsWith('image-'))) {
    assert.equal(step.if, '${{ !matrix.image.arch }}');
  }
  const sbom = build.steps.find(step => step.name === 'Generate SBOM (SPDX JSON)');
  const upload = build.steps.find(step => step.name === 'Upload SBOM');
  for (const value of [sbom.with.image, sbom.with['output-file'], upload.with.name, upload.with.path]) assert.match(value, /env.ARCH_SUFFIX/);
});

test('packaging waits for signed native manifests rather than per-arch receipts', () => {
  const jobs = parse(readFileSync(join(repo, '.github/workflows/release-public-interim.yml'), 'utf8')).jobs;
  const merge = jobs['merge-native-images'];
  assert.deepEqual(merge.needs, ['prepare', 'build-images']);
  assert.ok(jobs['cli-pack'].needs.includes('merge-native-images'));
  assert.equal(merge.permissions['id-token'], 'write');
  const index = predicate => merge.steps.findIndex(predicate);
  const manifest = index(step => step.id === 'manifest');
  const attest = index(step => step.uses?.startsWith('actions/attest-build-provenance'));
  const sign = index(step => step.run?.includes('cosign sign --yes'));
  const receipt = index(step => step.name === 'Record signed multi-arch image receipt');
  const upload = index(step => step.uses?.startsWith('actions/upload-artifact'));
  assert.ok(manifest >= 0 && attest > manifest && sign > attest && receipt > sign && upload > receipt);
  assert.equal(merge.steps[attest].with['subject-digest'], '${{ steps.manifest.outputs.digest }}');
  assert.match(merge.steps[sign].env.IMAGE, /@\$\{\{ steps.manifest.outputs.digest \}\}/);
  assert.equal(merge.steps[receipt].env.DIGEST, '${{ steps.manifest.outputs.digest }}');
  assert.equal(merge.steps[upload].with.name, 'image-${{ matrix.image }}-${{ env.VERSION }}');
  assert.equal(merge.steps[upload].with['if-no-files-found'], 'error');
  for (const step of merge.steps) assert.notEqual(step['continue-on-error'], true);
});

for (const name of ['kars-bridge-bff', 'kars-mcp-playwright']) test(`native manifest shell for ${name} rejects incomplete architectures and never publishes prerelease latest`, t => {
  const jobs = parse(readFileSync(join(repo, '.github/workflows/release-public-interim.yml'), 'utf8')).jobs;
  const script = jobs['merge-native-images'].steps.find(step => step.id === 'manifest').run;
  const root = fixture(t);
  const docker = `docker() {
    printf '%s\\n' "$*" >> "$DOCKER_CALLS"
    case "$*" in
      *--raw*) cat "$MANIFEST" ;;
      *--format*) printf '%s\\n' '${digest}' ;;
      *create*) return 0 ;;
      *) return 1 ;;
    esac
  }\n`;
  for (const [architectures, valid] of [
    [['amd64', 'arm64', 'unknown'], true],
    [['amd64'], false],
    [['arm64'], false],
    [['amd64', 'amd64', 'arm64'], false],
    [['amd64', 's390x'], false],
  ]) {
    for (const prerelease of ['true', 'false']) {
      const calls = join(root, 'calls');
      const output = join(root, 'output');
      writeFileSync(calls, '');
      writeFileSync(output, '');
      writeFileSync(join(root, 'manifest.json'), JSON.stringify({ manifests: architectures.map(architecture => ({
        platform: { os: architecture === 'unknown' ? 'unknown' : 'linux', architecture },
      })) }));
      const result = spawnSync('bash', ['-e', '-o', 'pipefail'], {
        input: docker + script, encoding: 'utf8',
        env: { ...process.env, REGISTRY: 'ghcr.io/azure', NAME: name, IMAGE_VERSION: 'v0.2.0-beta.3', PRERELEASE: prerelease,
          DOCKER_CALLS: calls, MANIFEST: join(root, 'manifest.json'), GITHUB_OUTPUT: output },
      });
      assert.equal(result.status === 0, valid, result.stderr);
      assert.equal(readFileSync(output, 'utf8'), valid ? `digest=${digest}\n` : '');
      const commands = readFileSync(calls, 'utf8');
      assert.equal(commands.includes(':latest'), prerelease === 'false');
      for (const arch of ['amd64', 'arm64']) assert.ok(commands.includes(`ghcr.io/azure/${name}:v0.2.0-beta.3-${arch}`));
      assert.ok(commands.includes(`ghcr.io/azure/${name}@${digest} --raw`));
    }
  }
});

test('locked dependency restoration registers the required Helm repository before building', () => {
  const jobs = parse(readFileSync(join(repo, '.github/workflows/release-public-interim.yml'), 'utf8')).jobs;
  const script = jobs['cli-pack'].steps.find(step => step.name === 'Restore exact locked chart dependencies before CLI bundling').run;
  const registration = 'helm repo add prometheus-community https://prometheus-community.github.io/helm-charts';
  assert.ok(script.includes(registration));
  assert.ok(script.indexOf(registration) < script.indexOf('helm dependency build deploy/helm/kars'));
  assert.ok(script.includes('git diff --exit-code -- deploy/helm/kars/Chart.lock'));
  assert.ok(script.includes('helm dependency build bridge/deploy/helm/kars-bridge'));
  assert.ok(!script.includes('helm dependency update'));
});

test('cold Helm configuration reproduces the failure and the actual workflow step restores exact locks', {
  skip: process.env.KARS_RELEASE_NETWORK_TESTS !== '1' && 'Set KARS_RELEASE_NETWORK_TESTS=1 to exercise the real public Helm repository',
}, t => {
  const root = fixture(t);
  for (const relative of ['deploy/helm/kars', 'deploy/helm/kars-datapath-witness', 'bridge/deploy/helm/kars-bridge']) {
    cpSync(join(repo, relative), join(root, relative), { recursive: true });
    rmSync(join(root, relative, 'charts'), { recursive: true, force: true });
  }
  const env = { ...process.env,
    HELM_CONFIG_HOME: join(root, '.helm/config'), HELM_CACHE_HOME: join(root, '.helm/cache'),
    HELM_DATA_HOME: join(root, '.helm/data'), HELM_PLUGINS: join(root, '.helm/plugins'),
    HELM_REPOSITORY_CONFIG: join(root, '.helm/config/repositories.yaml'),
    HELM_REPOSITORY_CACHE: join(root, '.helm/cache/repository'), HELM_REGISTRY_CONFIG: join(root, '.helm/config/registry.json'),
  };
  const lock = readFileSync(join(root, 'deploy/helm/kars/Chart.lock'), 'utf8');
  const failed = spawnSync('helm', ['dependency', 'build', 'deploy/helm/kars'], { cwd: root, env, encoding: 'utf8', timeout: 30000 });
  assert.notEqual(failed.status, 0);
  assert.match(failed.stderr, /no repository definition/);
  const jobs = parse(readFileSync(join(repo, '.github/workflows/release-public-interim.yml'), 'utf8')).jobs;
  const script = jobs['cli-pack'].steps.find(step => step.name === 'Restore exact locked chart dependencies before CLI bundling').run;
  const fixed = spawnSync('bash', ['-e', '-o', 'pipefail', '-c', script], { cwd: root, env, encoding: 'utf8', timeout: 180000 });
  assert.equal(fixed.status, 0, `${fixed.stderr}\n${fixed.stdout}`);
  assert.equal(readFileSync(join(root, 'deploy/helm/kars/Chart.lock'), 'utf8'), lock);
  for (const dependency of parse(lock).dependencies) {
    assert.ok(readFileSync(join(root, `deploy/helm/kars/charts/${dependency.name}-${dependency.version}.tgz`)).length > 0);
  }
});

test('packaging-only recovery skips every producer without permitting failed preparation or packaging', () => {
  const workflow = parse(readFileSync(join(repo, '.github/workflows/release-public-interim.yml'), 'utf8'));
  const jobs = workflow.jobs;
  assert.equal(workflow.on.workflow_dispatch.inputs.recovery_run_id.default, '');
  const validate = jobs.prepare.steps.find(step => step.id === 'recovery');
  assert.equal(validate.env.RECOVERY_RUN_ID, '${{ inputs.recovery_run_id }}');
  assert.match(validate.run, /release_recovery\.py prepare --run-id "\$RECOVERY_RUN_ID" --version "\$RELEASE_VERSION"/);
  assert.equal(jobs.prepare.outputs.recovery, "${{ steps.recovery.outputs.recovery || 'false' }}");
  const evaluate = (name, recovery, overrides = {}, cancelled = false) => {
    const job = jobs[name];
    const needs = Object.fromEntries([job.needs].flat().map(id => [id, {
      result: overrides[id] ?? (id === 'prepare' || id === 'cli-pack' || !recovery ? 'success' : 'skipped'),
      outputs: { recovery: String(recovery) },
    }]));
    const results = Object.values(needs).map(value => value.result);
    const condition = job.if.slice(3, -2).replace(/needs\.\*\.result/g, 'results').replace(/needs\.([\w-]+)/g, 'needs["$1"]');
    if (!/\b(?:always|success|failure|cancelled)\(/.test(condition) && (cancelled || results.some(result => result !== 'success'))) return false;
    return runInNewContext(condition, { needs, results, cancelled: () => cancelled, failure: () => results.includes('failure'), contains: (items, item) => items.includes(item) });
  };
  const producers = Object.keys(jobs).filter(name => !['prepare', 'cli-pack', 'release'].includes(name));
  for (const name of producers) {
    assert.equal(jobs[name].if, "${{ needs.prepare.outputs.recovery != 'true' }}", name);
    assert.equal(evaluate(name, false), true, `${name}: normal build`);
    assert.equal(evaluate(name, true), false, `${name}: recovery must never build`);
    assert.equal(evaluate(name, false, { prepare: 'failure' }), false, `${name}: invalid source`);
  }
  for (const name of ['cli-pack', 'release']) {
    assert.equal(evaluate(name, true), true, `${name}: successful recovery validation`);
    assert.equal(evaluate(name, false), true, `${name}: successful ordinary producers`);
    assert.equal(evaluate(name, true, { prepare: 'failure' }), false);
    assert.equal(evaluate(name, true, { prepare: 'skipped' }), false);
    assert.equal(evaluate(name, true, {}, true), false);
    assert.equal(evaluate(name, false, { 'build-images': 'failure' }), false);
    assert.equal(evaluate(name, false, { 'build-images': 'skipped' }), false);
    assert.equal(jobs[name].permissions.actions, 'read');
    assert.equal(jobs[name].env.RELEASE_RECOVERY_MANIFEST, '${{ needs.prepare.outputs.recoveryManifest }}');
  }
  for (const result of ['failure', 'skipped', 'cancelled']) assert.equal(evaluate('release', true, { 'cli-pack': result }), false);
  const downloads = [...jobs['cli-pack'].steps, ...jobs.release.steps].filter(step => step.uses?.startsWith('actions/download-artifact'));
  for (const step of downloads) {
    if (String(step.with.pattern ?? step.with.name).startsWith('packages-')) assert.equal(step.if, undefined, 'New packages must come from this recovery run');
    else assert.equal(step.if, "${{ needs.prepare.outputs.recovery != 'true' }}");
  }
  for (const [job, kind, destination] of [['cli-pack', 'images', 'image-receipts'], ['release', 'binaries', 'release-binaries'], ['release', 'sboms', 'release-artefacts']]) {
    const step = jobs[job].steps.find(step => step.run === `python3 ci/release_recovery.py download --kind ${kind} --destination ${destination}`);
    assert.equal(step.if, "${{ needs.prepare.outputs.recovery == 'true' }}");
    assert.equal(step.env.GH_TOKEN, '${{ secrets.GITHUB_TOKEN }}');
  }
});

test('release manifest distinguishes recovered producer provenance from current packaging provenance', t => {
  const root = fixture(t);
  mkdirSync(join(root, 'release-artefacts'));
  writeFileSync(join(root, 'release-artefacts/release-images.json'), JSON.stringify(images()));
  writeFileSync(join(root, 'release-artefacts/npm-publication.json'), JSON.stringify({ outcome: 'publish-failed' }));
  const jobs = parse(readFileSync(join(repo, '.github/workflows/release-public-interim.yml'), 'utf8')).jobs;
  const step = jobs.release.steps.find(step => step.name === 'Build release manifest from verified receipts and npm outcome');
  const script = step.run.split("<<'JS'\n")[1].split('\nJS')[0];
  const recovery = { producer: { source_sha: '1'.repeat(40), run_id: 123, run_attempt: 1 }, artifacts: [{ id: 456, digest }] };
  for (const binding of [null, recovery]) {
    rmSync(join(root, 'release-artefacts/release-recovery.json'), { force: true });
    const result = spawnSync(process.execPath, ['--input-type=module'], { cwd: root, input: script, encoding: 'utf8', env: {
      ...process.env, GITHUB_REPOSITORY: 'Azure/kars', GITHUB_SHA: '2'.repeat(40), GITHUB_REF: 'refs/heads/kars-bridge',
      GITHUB_RUN_ID: '789', GITHUB_RUN_ATTEMPT: '1', PRODUCER_SOURCE: binding ? recovery.producer.source_sha : '2'.repeat(40),
      PRODUCER_RUN_ID: binding ? '123' : '789', VERSION: 'v0.2.0-beta.6', RELEASE_VERSION: '0.2.0-beta.6', PRERELEASE: 'true',
      CLI_TARBALL: 'kars-runtime-cli-0.2.0-beta.6.tgz', RELEASE_RECOVERY_MANIFEST: binding ? JSON.stringify(binding) : '',
    } });
    assert.equal(result.status, 0, result.stderr);
    const manifest = JSON.parse(readFileSync(join(root, 'release-artefacts/release-manifest.json'), 'utf8'));
    assert.equal(manifest.commit, '2'.repeat(40));
    assert.equal(manifest.packaging.commit, manifest.commit);
    assert.equal(manifest.packaging.run_id, '789');
    if (binding) {
      assert.deepEqual(manifest.producer, recovery.producer);
      assert.deepEqual(manifest.recovery, { binding: 'release-recovery.json', rebuilt_producers: false });
      assert.deepEqual(JSON.parse(readFileSync(join(root, 'release-artefacts/release-recovery.json'), 'utf8')), recovery);
    } else {
      assert.equal(manifest.producer.source_sha, manifest.commit);
      assert.equal(manifest.producer.run_id, manifest.packaging.run_id);
      assert.equal(manifest.recovery, null);
      assert.ok(!readdirSync(join(root, 'release-artefacts')).includes('release-recovery.json'));
    }
  }
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
