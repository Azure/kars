// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { writeFile, unlink } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { WorkloadIdentityCredential } from '@azure/identity';
import { createHttpHeaders, type HttpClient } from '@azure/core-rest-pipeline';
import { describe, expect, it } from 'vitest';
import { TokenBroker } from '../src/aad';

const require = createRequire(import.meta.url);
const identityRequire = createRequire(require.resolve('@azure/identity'));
const tenantId = '22222222-2222-4222-8222-222222222222';
const clientId = '11111111-1111-4111-8111-111111111111';
const authority = `https://login.microsoftonline.com/${tenantId}`;

describe('maintained Azure Identity parent and MSAL compatibility', () => {
  it('ships the patched parent requirement to library consumers, without a root-only override', () => {
    const manifest = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
    const lock = JSON.parse(readFileSync(new URL('../package-lock.json', import.meta.url), 'utf8'));
    expect(manifest.dependencies['@azure/identity']).toBe('^4.14.1');
    expect(manifest.overrides).toBeUndefined();
    for (const metadata of Object.values(lock.packages) as { resolved?: string }[]) {
      if (!metadata.resolved) continue;
      const url = new URL(metadata.resolved);
      expect(url.origin).toBe('https://registry.npmjs.org');
      expect([url.username, url.password, url.search, url.hash]).toEqual(['', '', '', '']);
      expect(url.pathname).toMatch(/\.tgz$/);
    }
    expect(require('@azure/identity/package.json').dependencies['@azure/msal-node']).toBe('^7.1.0');
    expect(identityRequire('@azure/msal-node/package.json').version).toBe('7.2.0');
    for (const [name, version] of Object.entries({ '@azure/identity': '4.14.1', '@azure/msal-node': '7.2.0' })) {
      const copies = Object.entries(lock.packages).filter(([path]) => path.endsWith(`node_modules/${name}`));
      expect(copies).toHaveLength(1);
      expect(copies[0]?.[1]).toMatchObject({ version, resolved: `https://registry.npmjs.org/${name}/-/${name.split('/').pop()}-${version}.tgz`, integrity: expect.stringMatching(/^sha512-/) });
    }
  });

  it.each([false, true])('exchanges real workload assertions through Identity and MSAL (failure=%s)', async fail => {
    const tokenPath = resolve(`.identity-test-${randomUUID()}.jwt`);
    const assertion = 'test-only-federated-workload-assertion';
    const posts: URLSearchParams[] = [];
    const httpClient: HttpClient = {
      async sendRequest(request) {
        const url = new URL(request.url);
        expect(url.hostname).toBe('login.microsoftonline.com');
        let body: object;
        let status = 200;
        if (request.method === 'POST') {
          expect(url.pathname).toBe(`/${tenantId}/oauth2/v2.0/token`);
          const form = new URLSearchParams(String(request.body));
          posts.push(form);
          status = fail ? 401 : 200;
          body = fail
            ? { error: 'invalid_client', error_description: 'Test identity provider rejected workload assertion' }
            : { access_token: 'test-only-access-token', token_type: 'Bearer', expires_in: 3600 };
        } else if (url.pathname.includes('/discovery/instance')) {
          body = { tenant_discovery_endpoint: `${authority}/v2.0/.well-known/openid-configuration`, metadata: [{ preferred_network: 'login.microsoftonline.com', preferred_cache: 'login.microsoftonline.com', aliases: ['login.microsoftonline.com'] }] };
        } else {
          expect(url.pathname).toBe(`/${tenantId}/v2.0/.well-known/openid-configuration`);
          body = { authorization_endpoint: `${authority}/oauth2/v2.0/authorize`, token_endpoint: `${authority}/oauth2/v2.0/token`, issuer: `${authority}/v2.0`, jwks_uri: `${authority}/discovery/v2.0/keys` };
        }
        return { request, status, headers: createHttpHeaders({ 'content-type': 'application/json' }), bodyAsText: JSON.stringify(body) };
      },
    };
    await writeFile(tokenPath, assertion, { flag: 'wx', mode: 0o600 });
    try {
      const credential = new WorkloadIdentityCredential({ tenantId, clientId, tokenFilePath: tokenPath, httpClient, retryOptions: { maxRetries: 0 } });
      const broker = new TokenBroker(credential);
      const scope = 'https://graph.microsoft.com/.default';
      if (fail) {
        await expect(broker.getToken(scope)).rejects.toThrow(/invalid_client/);
      } else {
        await expect(broker.getToken(scope)).resolves.toBe('test-only-access-token');
        await expect(broker.getToken(scope)).resolves.toBe('test-only-access-token');
      }
      expect(posts).toHaveLength(1);
      expect(posts[0]?.get('client_id')).toBe(clientId);
      expect(posts[0]?.get('grant_type')).toBe('client_credentials');
      expect(posts[0]?.get('scope')).toBe(scope);
      expect(posts[0]?.get('client_assertion')).toBe(assertion);
      expect(posts[0]?.get('client_assertion_type')).toBe('urn:ietf:params:oauth:client-assertion-type:jwt-bearer');
      expect(posts[0]?.has('client_secret')).toBe(false);
    } finally {
      await unlink(tokenPath);
    }
  });
});
