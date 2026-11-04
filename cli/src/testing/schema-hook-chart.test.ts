// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.
import { execFileSync } from 'node:child_process';
import { createHash, createPrivateKey, X509Certificate } from 'node:crypto';
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
function render(args: string[] = [], release = 'kars', namespace = 'kars-system'): Json[] {
  const text = execFileSync('helm', ['template', release, chart, '--namespace', namespace,
    '--kube-version', '1.35.0', ...args], { encoding: 'utf8', timeout: 30000,
    stdio: ['ignore', 'pipe', 'pipe'], maxBuffer: 8 * 1024 * 1024 });
  return parseAllDocuments(text).map(document => {
    if (document.errors.length) throw document.errors[0];
    return document.toJSON();
  }).filter(Boolean);
}
const schemaHooks = (docs: Json[]) => docs.filter(doc => doc.metadata.name.includes('-schema-'));
function comparableResources(docs: Json[]): Json[] {
  const result = structuredClone(docs);
  const secret = result.find(doc => doc.kind === 'Secret' && doc.metadata.name === 'kars-metrics-server-tls')!;
  const metrics = result.find(doc => doc.kind === 'Deployment' && doc.metadata.name === 'kars-metrics-server')!;
  const api = result.find(doc => doc.kind === 'APIService' && doc.metadata.name === 'v1beta1.metrics.k8s.io')!;
  const ca = new X509Certificate(Buffer.from(secret.data['ca.crt'], 'base64'));
  const certificate = new X509Certificate(Buffer.from(secret.data['tls.crt'], 'base64'));
  expect(certificate.verify(ca.publicKey)).toBe(true);
  expect(certificate.checkPrivateKey(createPrivateKey(Buffer.from(secret.data['tls.key'], 'base64')))).toBe(true);
  expect(api.spec.caBundle).toBe(secret.data['ca.crt']);
  expect(metrics.spec.template.spec.volumes.find((volume: Json) => volume.name === 'serving-certificate').secret.defaultMode).toBe(0o444);
  const checksum = 'checksum/serving-certificate';
  expect(metrics.spec.template.metadata.annotations[checksum])
    .toBe(createHash('sha256').update(secret.data['tls.crt']).digest('hex'));
  // Separate offline renders generate independent certificates, even with identical values.
  for (const key of ['ca.crt', 'tls.crt', 'tls.key']) secret.data[key] = '<generated>';
  api.spec.caBundle = '<generated>';
  metrics.spec.template.metadata.annotations[checksum] = '<generated>';
  return result;
}
let baseline: Json[], documents: Json[], hooks: Json[], job: Json, identity: string;
beforeAll(() => {
  baseline = render(); documents = render(enabled); hooks = schemaHooks(documents);
  job = hooks.find(doc => doc.kind === 'Job')!; identity = job.metadata.name;
}, 30000);

describe('Core first-install schema stage', () => {
  it('is disabled by default and leaves every ordinary resource unchanged', () => {
    expect(schemaHooks(baseline)).toEqual([]);
    expect(baseline.some(doc => doc.metadata.labels?.['kars.azure.com/witness-addon'] === 'true')).toBe(true);
    expect(comparableResources(documents.filter(doc => !hooks.includes(doc))))
      .toEqual(comparableResources(baseline));
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
    expect(container.args).toEqual(['--release', 'kars', '--namespace', 'kars-system', '--revision', '1',
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
    expect(role.metadata.namespace).toBe('kars-system');
    expect(role.rules).toEqual([{ apiGroups: [''], resources: ['secrets'], verbs: ['get', 'list'] },
      { apiGroups: ['apps'], resources: ['deployments'], resourceNames: ['kars-controller'], verbs: ['get'] }]);
    for (const kind of ['RoleBinding', 'ClusterRoleBinding']) {
      const binding = hooks.find(doc => doc.kind === kind)!;
      expect(binding.roleRef.name).toBe(identity);
      expect(binding.subjects).toEqual([{ kind: 'ServiceAccount', name: identity, namespace: 'kars-system' }]);
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
    }, {
      key: 'kars.azure.com/metrics-managed', operator: 'NotIn', values: ['true'],
    }]);
    const deny = documents.find(doc => doc.kind === 'NetworkPolicy' && doc.metadata.name === 'kars-system-default-deny')!;
    expect(deny.spec).toEqual({ podSelector: {}, policyTypes: ['Ingress', 'Egress'], ingress: [], egress: [] });
    expect(operator.spec.egress.length).toBeGreaterThan(0);
    expect(operator.spec.ingress.length).toBeGreaterThan(0);
  });

  it.each([
    ['maintenance-core', 'kars-system'],
    ['kars', 'customer-core'],
    ['maintenance-core', 'customer-core'],
  ])('retains the composed Core witness ownership fence for %s/%s', (release, namespace) => {
    for (const managed of [true, false]) {
      expect(() => render([...enabled, '--set', `datapathWitness.managed=${managed}`], release, namespace))
        .toThrow('use the kars Core release or standalone kars-datapath-witness in kars-system');
    }
    for (const doc of hooks) expect(doc.metadata.name.length).toBeLessThanOrEqual(63);
  });

  it('allows the Cilium API identity only on reviewed ports without broadening other helper traffic', () => {
    const result = schemaHooks(render([...enabled,
      '--api-versions', 'cilium.io/v2/CiliumNetworkPolicy',
      '--set', 'schemaHook.apiDestinations[1].cidr=172.20.0.5/32',
      '--set', 'schemaHook.apiDestinations[1].port=6443',
      '--set', 'schemaHook.apiDestinations[2].cidr=172.20.0.6/32',
      '--set', 'schemaHook.apiDestinations[2].port=6443']));
    const helper = result.find(doc => doc.kind === 'Job')!;
    const policy = result.find(doc => doc.kind === 'CiliumNetworkPolicy')!;
    expect(policy).toBeDefined();
    expect(policy.metadata.namespace).toBe('kars-system');
    expect(policy.spec).toEqual({
      endpointSelector: { matchLabels: { 'kars.azure.com/schema-hook': helper.metadata.name } },
      egress: [{ toEntities: ['kube-apiserver'],
        toPorts: [{ ports: [{ port: '443', protocol: 'TCP' }, { port: '6443', protocol: 'TCP' }] }] }],
    });
    expect(policy.metadata.annotations['helm.sh/hook']).toBe('pre-install');
    expect(policy.metadata.annotations['helm.sh/hook-delete-policy']).toBe('before-hook-creation');
    expect(Number(policy.metadata.annotations['helm.sh/hook-weight']))
      .toBeLessThan(Number(helper.metadata.annotations['helm.sh/hook-weight']));
    expect(result.find(doc => doc.kind === 'NetworkPolicy')!.spec.ingress).toEqual([]);
    expect(hooks.some(doc => doc.kind === 'CiliumNetworkPolicy')).toBe(false);
    expect(helper.spec.template.spec.securityContext).toEqual(job.spec.template.spec.securityContext);
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


describe('Core atomic first-install SRE phase', () => {
  const sre = [...enabled, '--set', 'sre.enabled=true,inferenceBudget.enabled=true',
    '--set', 'sre.firstInstallFunding.tokens=1200,sre.firstInstallFunding.expiresAt=2999-01-01T00:00:00Z',
    '--set', `controller.image.digest=sha256:${'b'.repeat(64)}`,
    '--set', `runtimes.hermes.image=example.test/hermes:latest@sha256:${'c'.repeat(64)}`,
    '--set', `inferenceBudget.routerImageDigest=sha256:${'d'.repeat(64)}`,
    '--set', `inferenceRouter.image.tag=latest@sha256:${'d'.repeat(64)}`,
    '--set', 'inferenceBudget.caBundle=offline-public-ca,inferenceBudget.catalogVersion=offline-v1,inferenceBudget.tlsSecretName=operator-budget-tls',
    '--set-json', 'inferenceBudget.contracts=[{"id":"offline-render-contract"}]',
    '--set', 'sre.provider=operator-provider,sre.model=operator-model,sre.tokenBudget=123'];
  it('renders immutable complete intent instead of undiscoverable cold CRs and wires separately privileged bounded post-install work', () => {
    const docs = render(sre);
    expect(docs.filter(doc => doc.apiVersion.startsWith('kars.azure.com/') && !doc.metadata.annotations?.['helm.sh/hook'])).toEqual([]);
    expect(docs.some(doc => ['sre', 'sre-inference', 'sre-tools'].includes(doc.metadata.name))).toBe(false);
    const intentMap = docs.find(doc => doc.kind === 'ConfigMap' && doc.metadata.name === 'kars-sre-first-install')!;
    expect(intentMap.immutable).toBe(true);
    expect(intentMap.metadata.annotations['helm.sh/resource-policy']).toBe('keep');
    const intent = JSON.parse(intentMap.data['intent.json']);
    expect(intent.resources.map((r: Json) => r.kind).sort()).toEqual(['InferencePolicy', 'KarsSandbox', 'ToolPolicy']);
    expect(intent.funding).toEqual({ tokens: 1200, expiresAt: '2999-01-01T00:00:00Z' });
    expect(intent.valuesDigest).toMatch(/^[a-f0-9]{64}$/);
    expect(intent.budget).not.toHaveProperty('tls');
    expect(intent.resources.find((r: Json) => r.kind === 'InferencePolicy').spec.modelPreference.primary)
      .toEqual({ provider: 'operator-provider', deployment: 'operator-model' });
    expect(intent.resources.find((r: Json) => r.kind === 'KarsSandbox').metadata.annotations['kars.azure.com/sre-first-install-intent'])
      .toBe('kars-sre-first-install');
    const post = docs.filter(doc => doc.metadata.annotations?.['helm.sh/hook'] === 'post-install');
    const job = post.find(doc => doc.kind === 'Job')!;
    expect(job.spec.backoffLimit).toBe(0);
    expect(job.spec.activeDeadlineSeconds).toBe(660);
    expect(JSON.stringify(job.spec.template.spec.containers[0])).toContain('--phase');
    expect(JSON.stringify(job.spec.template.spec.containers[0])).toContain('sre');
    expect(post.some(doc => doc.kind === 'NetworkPolicy')).toBe(true);
    for (const role of post.filter(doc => ['Role', 'ClusterRole'].includes(doc.kind))) {
      for (const rule of role.rules) {
        expect(rule.verbs.some((v: string) => ['patch', 'update', 'delete', 'bind', 'escalate', '*'].includes(v))).toBe(false);
        if (rule.resources?.includes('customresourcedefinitions')) expect(rule.verbs).toEqual(['get']);
        if (rule.resources?.includes('secrets')) expect(rule.verbs.every((v: string) => ['get', 'list'].includes(v))).toBe(true);
      }
    }
  }, 30000);
  it.each(['sre.routerImageBaseline', 'sre.runtimeImageBaseline', 'missionDispatcher.imageBaseline', 'metricsServer.hostAliasesBaseline'])('rejects upgrade-only %s before cold SRE enrollment', field => {
    expect(() => render([...sre, '--set', `${field}=previous-workspace`])).toThrow('Cold SRE cannot use upgrade-only');
  });
  it.each(['builtInFunding', 'tokenTopUps', 'unboundedTokenAllowances'])('rejects previous-cluster %s before cold SRE enrollment', field => {
    expect(() => render([...sre, '--set-json', `inferenceBudget.${field}=[{"accountUid":"old-account"}]`])).toThrow('Cold SRE cannot reuse existing UID-bound funding');
  });
  it('cold-renders unbounded authority and only the catalog-patch and fund permissions it needs', () => {
    const args = [...sre];
    const index = args.findIndex(arg => arg.startsWith('sre.firstInstallFunding.tokens='));
    args.splice(index - 1, 2, '--set-json', 'sre.firstInstallFunding={"unboundedTokens":true}');
    const docs = render(args);
    const intent = JSON.parse(docs.find(doc => doc.kind === 'ConfigMap' && doc.metadata.name === 'kars-sre-first-install')!.data['intent.json']);
    expect(intent.funding).toEqual({ unboundedTokens: true });
    const role = docs.find(doc => doc.kind === 'Role' && doc.metadata.annotations?.['helm.sh/hook'] === 'post-install')!;
    expect(role.rules.filter((rule: Json) => rule.verbs.includes('patch'))).toEqual([
      { apiGroups: [''], resources: ['configmaps'], resourceNames: ['kars-inference-budget-contracts'], verbs: ['patch'] },
    ]);
    expect(role.rules.some((rule: Json) => rule.resources.includes('karsbudgetaccounts') && rule.verbs.includes('fund'))).toBe(true);
    const schema = docs.find(doc => doc.kind === 'CustomResourceDefinition' && doc.metadata.name === 'karssreregistrations.kars.azure.com')!.spec.versions[0].schema.openAPIV3Schema.properties.spec.properties.firstInstallFunding.properties.grant;
    expect(schema.required).toEqual(['root', 'limits']);
    expect(schema['x-kubernetes-validations'][0].rule).toContain('!has(self.expiresAt)');
  });
  it('binds complete specs that are unchanged by the actual CRD defaults', () => {
    const docs = render(sre);
    const intent = JSON.parse(docs.find(doc => doc.kind === 'ConfigMap'
      && doc.metadata.name === 'kars-sre-first-install')!.data['intent.json']);
    // Apply only defaults in the maintained schema, and only under present
    // objects: absent optional objects do not acquire their children's defaults.
    function defaulted(value: any, schema: Json): any {
      if (value === undefined || (value === null && !schema.nullable)) {
        if (!Object.hasOwn(schema, 'default')) return value;
        value = structuredClone(schema.default);
      }
      if (Array.isArray(value)) return value.map(item => defaulted(item, schema.items ?? {}));
      if (value === null || typeof value !== 'object') return value;
      const result = structuredClone(value);
      for (const [key, property] of Object.entries(schema.properties ?? {})) {
        const child = defaulted(result[key], property as Json);
        if (child !== undefined) result[key] = child;
      }
      return result;
    }
    for (const resource of intent.resources) {
      const crd = docs.find(doc => doc.kind === 'CustomResourceDefinition' && doc.spec.names.kind === resource.kind)!;
      const schema = crd.spec.versions.find((version: Json) => version.name === 'v1alpha1').schema.openAPIV3Schema;
      expect(defaulted(resource.spec, schema.properties.spec), resource.kind).toEqual(resource.spec);
      if (resource.kind === 'KarsSandbox') {
        const properties = schema.properties.spec.properties.sandbox.properties;
        expect(resource.spec.sandbox.writablePaths).toEqual(properties.writablePaths.default);
        expect(resource.spec.sandbox.selinuxContext).toBe('');
        for (const [key, property] of Object.entries(properties)) {
          if (!Object.hasOwn(property as Json, 'default') || ['isolation', 'selinuxContext'].includes(key)) continue;
          expect(resource.spec.sandbox[key], key).toEqual((property as Json).default);
        }
        // The old partial renderer acquires six fields at the API, including
        // a literal escaped SELinux string rather than Rust's empty default.
        const partial = { ...resource.spec, sandbox: { isolation: 'standard' } };
        const live = defaulted(partial, schema.properties.spec);
        expect(live.sandbox).toEqual({ isolation: 'standard', ...Object.fromEntries(
          Object.entries(properties).filter(([key, property]) => key !== 'isolation' && Object.hasOwn(property as Json, 'default'))
            .map(([key, property]) => [key, (property as Json).default]),
        ) });
        expect(live.sandbox.writablePaths).toEqual(['/sandbox', '/tmp']);
        expect(live.sandbox.selinuxContext).not.toBe('');
        expect(live).not.toEqual(partial);
      }
    }
  }, 30000);
  it.each([
    ['sre.firstInstallFunding.tokens=0', 'positive safe-integer'],
    ['sre.firstInstallFunding={}', 'firstInstallFunding'],
    ['controller.image.digest=', 'approved immutable'],
  ])('rejects incomplete authorization: %s', (override, message) => {
    expect(() => render([...sre, '--set', override])).toThrow(message);
  }, 30000);
});
