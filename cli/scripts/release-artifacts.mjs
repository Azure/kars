// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { readFileSync, writeFileSync, readdirSync, mkdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';

export function releaseVersion(input) {
  const match = /^v?((0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?)$/.exec(input);
  if (!match) throw new Error('Release version must be an explicit SemVer (optionally prefixed with v)');
  const version = match[1];
  const tag = `v${version}`;
  const imageTag = tag.replace('+', '_');
  if (imageTag.length > 128) throw new Error('Release version exceeds OCI tag length');
  return { version, tag, imageTag, prerelease: Boolean(match[5]), npmTag: match[5] ? 'next' : 'latest' };
}

export function stampCliVersion(directory, input) {
  const { version } = releaseVersion(input);
  for (const name of ['package.json', 'package-lock.json']) {
    const path = join(directory, name);
    const metadata = JSON.parse(readFileSync(path, 'utf8'));
    metadata.version = version;
    if (name === 'package-lock.json') metadata.packages[''].version = version;
    writeFileSync(path, JSON.stringify(metadata, null, 2) + '\n');
  }
}

// The two product charts consume these images; optional features remain opt-in.
export const coreImages = {
  'controller.image': 'kars-controller',
  'inferenceRouter.image': 'kars-inference-router',
  'sandbox.image': 'openclaw-sandbox',
  'headlamp.image': 'kars-headlamp',
  'metricsServer.image': 'kars-metrics-server',
  'agentMesh.registry.image': 'kars-agentmesh-registry',
  'agentMesh.relay.image': 'kars-agentmesh-relay',
  'a2aGateway.image': 'kars-a2a-gateway',
  'schemaHook.image': 'kars-schema-hook',
  'missionDispatcher.image': 'kars-mission-dispatcher',
  'evals.runnerImage': 'kars-conformance-runner',
  'managedMcp.playwrightImage': 'kars-mcp-playwright',
  'managedMcp.everythingImage': 'kars-mcp-everything',
  'runtimes.hermes.image': 'kars-runtime-hermes',
  'runtimes.openaiAgents.image': 'kars-runtime-openai-agents',
  'runtimes.mafPython.image': 'kars-runtime-maf-python',
  'runtimes.anthropic.image': 'kars-runtime-anthropic',
  'runtimes.langgraph.image': 'kars-runtime-langgraph',
  'runtimes.pydanticAi.image': 'kars-runtime-pydantic-ai',
  'datapathWitness.aggregator.image': 'kars-datapath-witness',
  'datapathWitness.gadget.image': 'kars-datapath-witness',
  'sandbox.seccompInstallerImage': 'seccomp-installer',
  'monitoring.prometheusOperator.image': 'kars-prometheus-operator',
  'monitoring.prometheusOperator.prometheusConfigReloader.image': 'kars-prometheus-config-reloader',
  'monitoring.prometheusOperator.admissionWebhooks.patch.image': 'kars-webhook-certgen',
  'monitoring.prometheus.prometheusSpec.image': 'kars-prometheus',
  'monitoring.kube-state-metrics.image': 'kars-kube-state-metrics',
  'monitoring.prometheus-node-exporter.image': 'kars-node-exporter',
  'monitoring.jsonExporter.image': 'kars-json-exporter',
  'monitoring.grafana.image': 'kars-grafana',
  'monitoring.grafana.sidecar.image': 'kars-k8s-sidecar',
};
export const bridgeImages = {
  'bff.image': 'kars-bridge-bff',
  'web.image': 'kars-bridge-web',
  'teamsGateway.image': 'kars-bridge-teams-gateway',
  'idp.dex.image': 'kars-bridge-idp',
};
export const requiredImages = [...new Set([...Object.values(coreImages), ...Object.values(bridgeImages), 'kars-sandbox-base'])];
const stringPaths = new Set([
  ...Object.keys(coreImages).filter(p => p.startsWith('runtimes.') || p.startsWith('datapathWitness.') || p.startsWith('managedMcp.')),
  'sandbox.seccompInstallerImage', 'evals.runnerImage', 'idp.dex.image',
]);

export function readReceipts(directory) {
  const images = {};
  for (const file of readdirSync(directory).filter(f => f.endsWith('.json'))) {
    const receipt = JSON.parse(readFileSync(join(directory, file), 'utf8'));
    if (!requiredImages.includes(receipt.name) || images[receipt.name]
      || !/^sha256:[a-f0-9]{64}$/.test(receipt.digest)
      || receipt.repository !== (receipt.name === 'seccomp-installer' ? 'mcr.microsoft.com/azurelinux/base/core' : `ghcr.io/azure/${receipt.name}`)) {
      throw new Error(`Invalid or duplicate image receipt: ${file}`);
    }
    images[receipt.name] = receipt;
  }
  for (const name of requiredImages) if (!images[name]) throw new Error(`Missing image receipt: ${name}`);
  return images;
}

function setValue(object, path, value) {
  const parts = path.split('.');
  let current = object;
  for (const key of parts.slice(0, -1)) current = current[key] ??= {};
  current[parts.at(-1)] = value;
}
function getValue(object, path) {
  return path.split('.').reduce((value, key) => value?.[key], object);
}

export function bindImages(values, bindings, images, onlyExisting = false) {
  for (const [path, name] of Object.entries(bindings)) {
    const old = getValue(values, path);
    if (onlyExisting && old === undefined) continue;
    const { repository, digest } = images[name];
    if (stringPaths.has(path)) {
      setValue(values, path, `${repository}:latest@${digest}`);
    } else {
      const image = { ...old, repository, tag: `latest@${digest}` };
      // Upstream monitoring subcharts separate registry and repository.
      if (old?.registry !== undefined) {
        image.registry = repository.split('/')[0];
        image.repository = repository.split('/').slice(1).join('/');
      }
      if (['controller.image', 'schemaHook.image', 'missionDispatcher.image'].includes(path)) image.digest = digest;
      if (path === 'schemaHook.image') image.tag = 'latest';
      setValue(values, path, image);
    }
  }
  if (bindings === coreImages && (!onlyExisting || getValue(values, 'inferenceBudget.routerImageDigest') !== undefined)) {
    setValue(values, 'inferenceBudget.routerImageDigest', images['kars-inference-router'].digest);
  }
  return values;
}

export async function prepareCharts(root, output, input, receipts) {
  const { parseDocument, stringify } = await import('yaml');
  const release = releaseVersion(input);
  const images = readReceipts(receipts);
  mkdirSync(output, { recursive: true });
  for (const [relative, bindings, product] of [
    ['deploy/helm/kars', coreImages, 'core'],
    ['bridge/deploy/helm/kars-bridge', bridgeImages, 'bridge'],
  ]) {
    const chart = join(root, relative);
    const metadataPath = join(chart, 'Chart.yaml');
    const metadata = parseDocument(readFileSync(metadataPath, 'utf8'));
    metadata.set('version', release.version);
    metadata.set('appVersion', release.version);
    writeFileSync(metadataPath, metadata.toString());
    for (const file of readdirSync(chart).filter(f => /^values(?:-.*)?\.yaml$/.test(f))) {
      const path = join(chart, file);
      const document = parseDocument(readFileSync(path, 'utf8'));
      const original = document.toJS();
      const values = bindImages(structuredClone(original), bindings, images, file !== 'values.yaml');
      // Preserve comments and all non-image policy/identity settings in packaged profiles.
      for (const key of [...Object.keys(bindings), 'inferenceBudget.routerImageDigest']) {
        const value = getValue(values, key);
        if (value !== undefined && JSON.stringify(value) !== JSON.stringify(getValue(original, key))) document.setIn(key.split('.'), value);
      }
      writeFileSync(path, document.toString());
    }
    const defaults = parseDocument(readFileSync(join(chart, 'values.yaml'), 'utf8')).toJS();
    const overlay = {};
    for (const key of [...Object.keys(bindings), ...(product === 'core' ? ['inferenceBudget.routerImageDigest'] : [])]) setValue(overlay, key, getValue(defaults, key));
    writeFileSync(join(output, `${product}-release-images.yaml`), stringify(overlay));
  }
  writeFileSync(join(output, 'release-images.json'), JSON.stringify(Object.values(images), null, 2) + '\n');
  return release;
}

async function main([command, ...args]) {
  if (command === 'version' && args.length === 1) {
    for (const [key, value] of Object.entries(releaseVersion(args[0]))) console.log(`${key}=${value}`);
  } else if (command === 'prepare' && args.length === 4) {
    await prepareCharts(...args);
  } else if (command === 'cli-version' && args.length === 2) {
    stampCliVersion(...args);
  } else {
    throw new Error('Usage: release-artifacts.mjs version VERSION | cli-version DIRECTORY VERSION | prepare ROOT OUTPUT VERSION RECEIPTS');
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
}
