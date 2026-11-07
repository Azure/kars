// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test, { after, before } from 'node:test';
import { kindBetaKubeletTrust, kindNodeKubeletTrust } from '../../cli/dist/lib/kind-kubelet-trust.js';
import { clusterValues, values } from './addon-images.mjs';

const cluster = 'kars-e2e';
const names = [`${cluster}-control-plane`, `${cluster}-worker`];
let directory;
const chains = [], authorities = [];
before(() => {
  directory = mkdtempSync(path.join(tmpdir(), 'kars-kubelet-trust-'));
  const openssl = args => execFileSync('openssl', args, { cwd: directory, stdio: 'pipe' });
  for (const [index, name] of names.entries()) {
    openssl(['req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes', '-keyout', `${index}-ca.key`, '-out', `${index}-ca.pem`, '-days', '2', '-subj', `/CN=${name}-ca`, '-addext', 'basicConstraints=critical,CA:TRUE']);
    openssl(['req', '-new', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes', '-keyout', `${index}-leaf.key`, '-out', `${index}-leaf.csr`, '-subj', `/CN=${name}`]);
    writeFileSync(path.join(directory, `${index}-extensions`), `subjectAltName=DNS:${name}\nbasicConstraints=critical,CA:FALSE\n`);
    openssl(['x509', '-req', '-in', `${index}-leaf.csr`, '-CA', `${index}-ca.pem`, '-CAkey', `${index}-ca.key`, '-CAcreateserial', '-out', `${index}-leaf.pem`, '-days', '1', '-extfile', `${index}-extensions`]);
    authorities.push(readFileSync(path.join(directory, `${index}-ca.pem`), 'utf8').trim());
    chains.push(readFileSync(path.join(directory, `${index}-leaf.pem`), 'utf8') + '\n' + authorities[index]);
  }
});
after(() => { if (directory) rmSync(directory, { recursive: true, force: true }); });

function fixture() {
  const nodes = names.map((name, index) => ({ metadata: { name, uid: `node-${index}` }, status: { addresses: [
    { type: 'InternalIP', address: `172.18.0.${index + 2}` }, { type: 'Hostname', address: name },
  ] } }));
  const containers = names.map((name, index) => ({ Name: `/${name}`, State: { Running: true },
    Config: { Labels: { 'io.x-k8s.kind.cluster': cluster, 'io.x-k8s.kind.role': index ? 'worker' : 'control-plane' } },
    NetworkSettings: { Networks: { kind: { IPAddress: `172.18.0.${index + 2}` } } } }));
  const calls = [];
  let reads = 0;
  const result = { nodes, containers, chains: [...chains], calls, after: null };
  result.execute = (file, args, capture) => {
    calls.push([file, args]);
    assert.equal(capture, true);
    if (file === 'kubectl') {
      assert.deepEqual(args, ['--context', `kind-${cluster}`, 'get', 'nodes', '-o', 'json']);
      return JSON.stringify({ items: reads++ && result.after ? result.after : result.nodes });
    }
    assert.equal(file, 'docker');
    const index = names.indexOf(args[1]);
    assert.ok(index >= 0);
    if (args[0] === 'inspect') { assert.equal(args.length, 2); return JSON.stringify([result.containers[index]]); }
    assert.deepEqual(args, ['exec', names[index], 'cat', '/var/lib/kubelet/pki/kubelet.crt']);
    return result.chains[index];
  };
  return result;
}

test('collects independently verified public trust for both fresh Kind nodes', () => {
  const f = fixture();
  const result = clusterValues(f.execute);
  assert.equal(f.calls.length, 6);
  assert.deepEqual(result.metricsServer.hostAliases, names.map((name, index) => ({ ip: `172.18.0.${index + 2}`, hostnames: [name] })));
  assert.deepEqual(result.metricsServer.kubelet, { insecureTLS: false, preferredAddressTypes: ['Hostname'], caBundle: authorities.map(ca => `${ca}\n`).join('\n') });
  delete result.metricsServer.hostAliases;
  delete result.metricsServer.kubelet;
  assert.deepEqual(result, values());
});

test('keeps the customer beta helper restricted to its single control-plane', () => {
  const f = fixture();
  assert.deepEqual(kindBetaKubeletTrust(cluster, f.nodes[0], f.containers[0], chains[0]), kindNodeKubeletTrust(cluster, f.nodes[0], f.containers[0], chains[0]));
  assert.throws(() => kindBetaKubeletTrust(cluster, f.nodes[1], f.containers[1], chains[1]), /owned control-plane/);
});

test('rejects missing, foreign, duplicate and extra nodes before container access', () => {
  for (const mutate of [f => f.nodes.pop(), f => f.nodes.push(f.nodes[0]), f => f.nodes[1] = f.nodes[0], f => f.nodes[0].metadata.name = 'foreign-control-plane']) {
    const f = fixture(); mutate(f);
    assert.throws(() => clusterValues(f.execute), /Unexpected E2E Kind nodes/);
    assert.equal(f.calls.length, 1);
  }
});

test('rejects absent or duplicate inspect results before reading certificates', () => {
  for (const containers of [[], [{}, {}]]) {
    const f = fixture();
    const execute = (file, args, capture) => file === 'docker' && args[0] === 'inspect'
      ? JSON.stringify(containers) : f.execute(file, args, capture);
    assert.throws(() => clusterValues(execute), /Missing E2E Kind node container/);
    assert.equal(f.calls.length, 1);
  }
});

test('rejects changed identity, address, ownership and stopped node containers', () => {
  for (const mutate of [
    f => f.containers[1].Config.Labels['io.x-k8s.kind.cluster'] = 'foreign',
    f => f.containers[1].Config.Labels['io.x-k8s.kind.role'] = 'control-plane',
    f => f.containers[1].State.Running = false,
    f => f.containers[1].NetworkSettings.Networks.kind.IPAddress = '172.18.0.99',
    f => f.nodes[1].status.addresses[1].address = 'foreign-worker',
    f => { f.after = structuredClone(f.nodes); f.after[1].metadata.uid = 'replacement'; },
    f => { f.after = structuredClone(f.nodes); f.after[1].status.addresses[0].address = '172.18.0.99'; },
    f => { f.nodes[1].status.addresses[0].address = '172.18.0.2'; f.containers[1].NetworkSettings.Networks.kind.IPAddress = '172.18.0.2'; },
  ]) {
    const f = fixture(); mutate(f);
    assert.throws(() => clusterValues(f.execute), /owned|changed|overlap/);
  }
});

test('rejects cross-node chains, untrusted issuers, expired certificates and private material', () => {
  const f = fixture();
  const privateKey = readFileSync(path.join(directory, '1-leaf.key'), 'utf8');
  for (const pem of [chains[0], chains[1].replace(authorities[1], authorities[0]), chains[1] + privateKey, authorities[1]]) {
    f.chains[1] = pem;
    assert.throws(() => clusterValues(f.execute), /certificate|public/);
  }
  assert.throws(() => kindNodeKubeletTrust(cluster, f.nodes[1], f.containers[1], chains[1], 0), /validity/);
  assert.throws(() => kindNodeKubeletTrust(cluster, f.nodes[1], f.containers[1], chains[1], Date.now() + 3 * 86400000), /validity/);
});

test('actual Helm chart projects all verified CAs and hostnames without insecure TLS', () => {
  const { parseAllDocuments } = createRequire(new URL('../../cli/package.json', import.meta.url))('yaml');
  const override = clusterValues(fixture().execute);
  const rendered = execFileSync('helm', ['template', 'kars', 'deploy/helm/kars', '--namespace', 'kars-system', '--values', '-'], { input: JSON.stringify(override), encoding: 'utf8' });
  const documents = parseAllDocuments(rendered).map(doc => { assert.equal(doc.errors.length, 0); return doc.toJSON(); }).filter(Boolean);
  const deployment = documents.find(doc => doc.kind === 'Deployment' && doc.metadata.name === 'kars-metrics-server');
  const ca = documents.find(doc => doc.kind === 'ConfigMap' && doc.metadata.name === 'kars-metrics-server-kubelet-ca');
  assert.deepEqual(deployment.spec.template.spec.hostAliases, override.metricsServer.hostAliases);
  assert.equal(ca.data['ca.crt'], override.metricsServer.kubelet.caBundle);
  const container = deployment.spec.template.spec.containers[0];
  assert.ok(container.args.includes('--kubelet-preferred-address-types=Hostname'));
  assert.ok(container.args.includes('--kubelet-certificate-authority=/var/run/kubelet-ca/ca.crt'));
  assert.ok(!container.args.includes('--kubelet-insecure-tls'));
  assert.equal(container.imagePullPolicy, 'Never');
});
