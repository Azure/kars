// Copyright (c) Microsoft Corporation. Licensed under the MIT License.
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { beforeAll, describe, expect, it } from 'vitest';
import { parseAllDocuments } from 'yaml';

type Json = Record<string, any>;
const root = fileURLToPath(new URL('../../../', import.meta.url));
const chart = `${root}deploy/helm/kars`;
const enabled = ['--set', 'schemaHook.enabled=true',
  '--set', 'schemaHook.image.repository=example.test/schema-toolchain',
  '--set', `schemaHook.image.digest=sha256:${'a'.repeat(64)}`,
  '--set', 'schemaHook.apiDestinations[0].cidr=10.96.0.1/32',
  '--set', 'schemaHook.apiDestinations[0].port=443'];
function render(args: string[] = [], release = 'kars', namespace = 'test-core'): Json[] {
  const text = execFileSync('helm', ['template', release, chart, '--namespace', namespace,
    '--kube-version', '1.35.0', ...args], { encoding: 'utf8', timeout: 30000,
    stdio: ['ignore', 'pipe', 'pipe'], maxBuffer: 8 * 1024 * 1024 });
  return parseAllDocuments(text).map(document => {
    if (document.errors.length) throw document.errors[0];
    return document.toJSON();
  }).filter(Boolean);
}
const schemaHooks = (docs: Json[]) => docs.filter(doc => doc.metadata.name.includes('-schema-'));
let baseline: Json[], documents: Json[], hooks: Json[], job: Json, identity: string;
beforeAll(() => {
  baseline = render(); documents = render(enabled); hooks = schemaHooks(documents);
  job = hooks.find(doc => doc.kind === 'Job')!; identity = job.metadata.name;
}, 30000);

describe('Core first-install schema stage', () => {
  it('is disabled by default and leaves every ordinary resource unchanged', () => {
    expect(schemaHooks(baseline)).toEqual([]);
    expect(documents.filter(doc => !hooks.includes(doc))).toEqual(baseline);
    expect(hooks.map(doc => doc.kind).sort()).toEqual(['ServiceAccount', 'ClusterRole', 'ClusterRoleBinding',
      'Role', 'RoleBinding', 'ConfigMap', 'NetworkPolicy', 'Job'].sort());
    for (const crd of documents.filter(doc => doc.kind === 'CustomResourceDefinition')) {
      expect(crd.metadata.annotations['helm.sh/hook']).toBeUndefined();
      expect(crd.metadata.annotations['helm.sh/resource-policy']).toBe('keep');
    }
    expect(hooks.some(doc => doc.kind === 'Namespace' || doc.apiVersion.startsWith('kars.azure.com/'))).toBe(false);
  });

  it('runs before ordinary resources and retains isolation, credentials and evidence across watch failures', () => {
    for (const doc of hooks) {
      expect(doc.metadata.annotations['helm.sh/hook']).toBe('pre-install');
      expect(doc.metadata.annotations['helm.sh/hook-delete-policy']).toBe('before-hook-creation');
      if (doc !== job) {
        expect(Number(doc.metadata.annotations['helm.sh/hook-weight'])).toBeLessThan(Number(job.metadata.annotations['helm.sh/hook-weight']));
      }
    }
    expect(job.metadata.annotations['helm.sh/hook-delete-policy']).toBe('before-hook-creation');
    expect(job.spec.backoffLimit).toBe(0);
    expect(job.spec.activeDeadlineSeconds).toBe(180);
    expect(job.spec.template.spec.restartPolicy).toBe('Never');
    expect(job.spec.ttlSecondsAfterFinished).toBeUndefined();
    expect(schemaHooks(render(enabled))[0].metadata.name).not.toBe(identity);
  });

  it('binds the actual bundle, release and outer installer version to the mounted entrypoint', () => {
    const program = hooks.find(doc => doc.kind === 'ConfigMap')!;
    expect(program.immutable).toBe(true);
    expect(program.data).toBeUndefined();
    expect(Buffer.from(program.binaryData['schema-hook.mjs'], 'base64'))
      .toEqual(readFileSync(`${chart}/files/schema-hook.mjs`));
    const pod = job.spec.template.spec;
    expect(pod.nodeSelector).toEqual({ 'kubernetes.io/os': 'linux', 'kubernetes.io/arch': 'amd64' });
    const container = pod.containers[0];
    expect(container.command).toEqual(['/usr/local/bin/node', '/opt/kars-schema/schema-hook.mjs']);
    expect(container.args).toEqual(['--release', 'kars', '--namespace', 'test-core', '--revision', '1',
      '--operation', 'install', '--installer-helm-version', expect.stringMatching(/^v4\.1\.3(?:\+.*)?$/), '--timeout-ms', '120000']);
    expect(container.image).toBe(`example.test/schema-toolchain:latest@sha256:${'a'.repeat(64)}`);
    expect(pod.volumes.find((volume: Json) => volume.name === 'program').configMap).toEqual({ name: identity, defaultMode: 292 });
    expect(container.volumeMounts).toContainEqual({ name: 'program', mountPath: '/opt/kars-schema', readOnly: true });
  });

  it('uses restricted nonroot execution and short-lived explicit in-cluster credentials', () => {
    const pod = job.spec.template.spec;
    expect(pod.securityContext).toEqual({ runAsNonRoot: true, runAsUser: 10001, runAsGroup: 10001,
      fsGroup: 10001, seccompProfile: { type: 'RuntimeDefault' } });
    expect(pod.automountServiceAccountToken).toBe(false);
    expect(pod.enableServiceLinks).toBe(false);
    expect(hooks.find(doc => doc.kind === 'ServiceAccount')!.automountServiceAccountToken).toBe(false);
    expect(pod.containers[0].securityContext).toEqual({ allowPrivilegeEscalation: false,
      readOnlyRootFilesystem: true, capabilities: { drop: ['ALL'] } });
    const token = pod.volumes.find((volume: Json) => volume.name === 'api-identity').projected;
    expect(token.defaultMode).toBe(292);
    expect(token.sources).toEqual([{ serviceAccountToken: { path: 'token', expirationSeconds: 600 } },
      { configMap: { name: 'kube-root-ca.crt', items: [{ key: 'ca.crt', path: 'ca.crt' }] } },
      { downwardAPI: { items: [{ path: 'namespace', fieldRef: { fieldPath: 'metadata.namespace' } }] } }]);
    expect(pod.volumes.find((volume: Json) => volume.name === 'temporary').emptyDir).toEqual({ sizeLimit: '128Mi' });
  });

  it('grants only schema mutation and explicit read capabilities, never namespace or workload writes', () => {
    const cluster = hooks.find(doc => doc.kind === 'ClusterRole')!;
    const crds = cluster.rules.find((rule: Json) => rule.resourceNames && rule.resources.includes('customresourcedefinitions'));
    expect([...crds.resourceNames].sort()).toEqual(documents.filter(doc => doc.kind === 'CustomResourceDefinition').map(doc => doc.metadata.name).sort());
    expect(crds.verbs).toEqual(['get', 'patch']);
    for (const rule of cluster.rules.filter((rule: Json) => rule.verbs.some((verb: string) => !['get', 'list'].includes(verb)))) {
      expect(rule.resources).toEqual(['customresourcedefinitions']);
      expect(rule.verbs.every((verb: string) => ['get', 'create', 'patch'].includes(verb))).toBe(true);
    }
    const role = hooks.find(doc => doc.kind === 'Role')!;
    expect(role.metadata.namespace).toBe('test-core');
    expect(role.rules).toEqual([{ apiGroups: [''], resources: ['secrets'], verbs: ['get', 'list'] },
      { apiGroups: ['apps'], resources: ['deployments'], resourceNames: ['kars-controller'], verbs: ['get'] }]);
    for (const kind of ['RoleBinding', 'ClusterRoleBinding']) {
      const binding = hooks.find(doc => doc.kind === kind)!;
      expect(binding.roleRef.name).toBe(identity);
      expect(binding.subjects).toEqual([{ kind: 'ServiceAccount', name: identity, namespace: 'test-core' }]);
    }
  });

  it('permits only the reviewed API endpoint and excludes the helper from ordinary operator egress', () => {
    const policy = hooks.find(doc => doc.kind === 'NetworkPolicy')!;
    expect(policy.spec).toEqual({ podSelector: { matchLabels: { 'kars.azure.com/schema-hook': identity } },
      policyTypes: ['Ingress', 'Egress'], ingress: [],
      egress: [{ to: [{ ipBlock: { cidr: '10.96.0.1/32' } }], ports: [{ protocol: 'TCP', port: 443 }] }] });
    expect(job.spec.template.metadata.labels['app.kubernetes.io/component']).toBe('schema-hook');
    const operator = documents.find(doc => doc.kind === 'NetworkPolicy' && doc.metadata.name === 'kars-system-operator-allow')!;
    expect(operator.spec.podSelector.matchExpressions).toEqual([{
      key: 'app.kubernetes.io/component', operator: 'NotIn', values: ['schema-hook'],
    }]);
    const deny = documents.find(doc => doc.kind === 'NetworkPolicy' && doc.metadata.name === 'kars-system-default-deny')!;
    expect(deny.spec).toEqual({ podSelector: {}, policyTypes: ['Ingress', 'Egress'], ingress: [], egress: [] });
    expect(operator.spec.egress.length).toBeGreaterThan(0);
    expect(operator.spec.ingress.length).toBeGreaterThan(0);
  });

  it('supports bounded distinct release/namespace names without a hardcoded customer Namespace', () => {
    const result = schemaHooks(render(enabled, 'maintenance-core', 'customer-core'));
    for (const doc of result) {
      expect(doc.metadata.name.startsWith('maintenance-core-schema-')).toBe(true);
      expect(doc.metadata.name.length).toBeLessThanOrEqual(63);
      if (doc.metadata.namespace) expect(doc.metadata.namespace).toBe('customer-core');
    }
    expect(result.find(doc => doc.kind === 'Job')!.spec.template.spec.containers[0].args.slice(0, 4))
      .toEqual(['--release', 'maintenance-core', '--namespace', 'customer-core']);
  });

  it('selects an explicitly qualified native ARM helper without relaxing its security profile', () => {
    const native = schemaHooks(render([...enabled, '--set', 'schemaHook.architecture=arm64']))
      .find(doc => doc.kind === 'Job')!.spec.template.spec;
    expect(native.nodeSelector).toEqual({ 'kubernetes.io/os': 'linux', 'kubernetes.io/arch': 'arm64' });
    expect(native.securityContext).toEqual(job.spec.template.spec.securityContext);
    expect(native.containers[0].resources).toEqual(job.spec.template.spec.containers[0].resources);
    expect(native.containers[0].command).toEqual(job.spec.template.spec.containers[0].command);
    expect(() => render([...enabled, '--set', 'schemaHook.architecture=unsupported'])).toThrow();
    expect(() => render([...enabled, '--set', 'schemaHook.architecture='])).toThrow();
  });

  it('refuses simultaneous identity activation in the schema hook itself', () => {
    expect(() => render([...enabled, '--skip-schema-validation', '--set', 'entraSidecar.enabled=true']))
      .toThrow('schemaHook requires entraSidecar.enabled=false');
  });

  it.each([
    ['--is-upgrade'], ['--kube-version', '1.34.0'], ['--set', 'policyPrototype.enabled=true'],
    ['--set', 'schemaHook.image.repository='], ['--set', 'schemaHook.image.digest='],
    ['--set', 'schemaHook.image.digest=sha256:bad'], ['--set', 'schemaHook.image.tag=v1'],
    ['--set', 'schemaHook.apiDestinations=[]'], ['--set', 'schemaHook.apiDestinations[0].cidr=0.0.0.0/0'],
    ['--set', 'schemaHook.apiDestinations[0].cidr=999.1.1.1/32'], ['--set', 'schemaHook.apiDestinations[0].port=0'],
    ['--set', 'schemaHook.apiDestinations[0].port=65536'], ['--set', 'schemaHook.apiDestinations[0].port=443.5'],
    ['--set', 'schemaHook.apiDestinations[0].unreviewed=true'],
  ])('refuses an unsupported mode or input: %j', (...args) => {
    expect(() => render([...enabled, ...args])).toThrow();
  });
});
