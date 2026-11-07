// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { MONITORING_IMAGE_TARGETS, METRICS_SERVER_IMAGE_TARGET } from '../../cli/dist/lib/image-targets.js';
import { kindNodeKubeletTrust } from '../../cli/dist/lib/kind-kubelet-trust.js';

export const targets = [
  ...MONITORING_IMAGE_TARGETS,
  METRICS_SERVER_IMAGE_TARGET,
  { repo: 'kars-headlamp', dockerfile: 'sandbox-images/headlamp/Dockerfile' },
];
// Both generated config-reloader containers inherit prometheusSpec.image.pullPolicy.
export const policies = [
  'headlamp.image.pullPolicy', 'metricsServer.image.pullPolicy',
  'monitoring.prometheusOperator.image.pullPolicy',
  'monitoring.prometheusOperator.admissionWebhooks.patch.image.pullPolicy',
  'monitoring.prometheus.prometheusSpec.image.pullPolicy',
  'monitoring.kube-state-metrics.image.pullPolicy',
  'monitoring.prometheus-node-exporter.image.pullPolicy',
  'monitoring.jsonExporter.image.pullPolicy',
];
const receiptPath = '.e2e-addon-images.json';
const valuesPath = '.e2e-addon-values.json';
const reference = target => `ghcr.io/azure/${target.repo}:latest`;
const run = (file, args, capture = false) => execFileSync(file, args, {
  stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit', encoding: 'utf8',
});
const imageId = (execute, image) => {
  const id = execute('docker', ['image', 'inspect', '--format', '{{.Id}}', image], true).trim();
  if (!/^sha256:[a-f0-9]{64}$/.test(id)) throw new Error('Invalid local add-on image identity');
  return id;
};

export function build(execute = run, prebuiltHeadlamp = false) {
  const images = [];
  for (const target of targets) {
    const image = reference(target);
    if (target.repo === 'kars-headlamp' && prebuiltHeadlamp) {
      imageId(execute, 'kars-headlamp:latest');
      execute('docker', ['tag', 'kars-headlamp:latest', image]);
    } else {
      execute('docker', ['build', '--tag', image, '--file', target.dockerfile,
        ...(target.target ? ['--target', target.target] : []), '.']);
    }
    images.push({ image, id: imageId(execute, image) });
  }
  return images;
}

export function verify(images, execute = run) {
  if (!Array.isArray(images) || images.length !== targets.length
      || images.some((item, index) => item.image !== reference(targets[index])
        || !/^sha256:[a-f0-9]{64}$/.test(item.id))) {
    throw new Error('Incomplete or unexpected add-on image inventory');
  }
  for (const item of images) {
    if (imageId(execute, item.image) !== item.id) throw new Error('Add-on image changed after preparation');
  }
}

export function load(images, execute = run) {
  // Verify the complete inventory before mutating the disposable cluster.
  verify(images, execute);
  execute('kind', ['load', 'docker-image', ...images.map(item => item.image), '--name', 'kars-e2e']);
}

export function values() {
  const result = {};
  for (const key of policies) {
    const parts = key.split('.');
    const leaf = parts.pop();
    let node = result;
    for (const part of parts) node = node[part] ??= {};
    node[leaf] = 'Never';
  }
  return result;
}

export function clusterValues(execute = run) {
  const cluster = 'kars-e2e';
  const expected = [`${cluster}-control-plane`, `${cluster}-worker`];
  const nodes = () => {
    const result = JSON.parse(execute('kubectl', ['--context', `kind-${cluster}`, 'get', 'nodes', '-o', 'json'], true));
    if (!Array.isArray(result.items) || result.items.length !== expected.length) throw new Error('Unexpected E2E Kind nodes');
    const sorted = [...result.items].sort((a, b) => String(a.metadata?.name).localeCompare(String(b.metadata?.name)));
    if (sorted.some((node, index) => node.metadata?.name !== expected[index])) throw new Error('Unexpected E2E Kind nodes');
    return sorted;
  };
  const before = nodes();
  const trusts = before.map(node => {
    const name = node.metadata.name;
    const containers = JSON.parse(execute('docker', ['inspect', name], true));
    if (!Array.isArray(containers) || containers.length !== 1) throw new Error('Missing E2E Kind node container');
    const pem = execute('docker', ['exec', name, 'cat', '/var/lib/kubelet/pki/kubelet.crt'], true);
    return kindNodeKubeletTrust(cluster, node, containers[0], pem);
  });
  const identity = items => items.map(node => ({ name: node.metadata.name, uid: node.metadata.uid, addresses: node.status?.addresses }));
  if (JSON.stringify(identity(before)) !== JSON.stringify(identity(nodes()))) throw new Error('E2E Kind nodes changed during trust collection');
  const hostAliases = trusts.flatMap(trust => trust.hostAliases);
  if (new Set(hostAliases.map(alias => alias.ip)).size !== expected.length) throw new Error('E2E Kind node addresses overlap');
  const result = values();
  result.metricsServer.hostAliases = hostAliases;
  result.metricsServer.kubelet = { insecureTLS: false, preferredAddressTypes: ['Hostname'],
    caBundle: [...new Set(trusts.map(trust => trust.kubelet.caBundle))].join('\n') };
  return result;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const command = process.argv[2];
  const revision = process.env.GITHUB_SHA || run('git', ['rev-parse', 'HEAD'], true).trim();
  if (command === 'build') {
    const images = build(run, process.env.KARS_E2E_PREBUILT_HEADLAMP === '1');
    writeFileSync(receiptPath, JSON.stringify({ revision, images }, null, 2));
  } else if (command === 'load' || command === 'export') {
    const receipt = JSON.parse(readFileSync(receiptPath, 'utf8'));
    if (receipt.revision !== revision) throw new Error('Add-on artifacts belong to another source revision');
    if (command === 'load') {
      load(receipt.images);
      writeFileSync(valuesPath, JSON.stringify(clusterValues(), null, 2));
    } else {
      verify(receipt.images);
      run('docker', ['save', '--output', '.e2e-addon-images.tar', ...receipt.images.map(item => item.image)]);
    }
  } else {
    throw new Error('Expected build, load or export');
  }
}
