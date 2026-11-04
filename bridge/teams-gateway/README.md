<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Teams gateway

The gateway uses the Microsoft Teams SDK for Bot Framework authentication and
outbound bot tokens. `src/main.ts` supplies the configured application ID,
client secret and tenant ID to `App`; inbound service-token verification remains
in the SDK. Gateway identity mapping and internal HMAC verification remain
additional authorization boundaries, not replacements for Teams authentication.

## MSAL security compatibility pin

The gateway scopes an npm override to `@microsoft/teams.apps` to resolve
`@azure/msal-node` **7.1.0** (and its declared `@azure/msal-common` 16.14.2).
[GHSA-xcx6-jx8p-x3h5](https://github.com/advisories/GHSA-xcx6-jx8p-x3h5)
affects MSAL Node >=5.0.0-beta.0 and <7.1.0. The fix sanitizes confidential-client
authorization-code payloads and rejects unsupported instance-aware inputs.
No advisory is suppressed, and no MSAL code is patched or vendored here.

Compatibility basis, checked 2026-11-04:

- Installed Teams SDK 2.0.15 declares MSAL `^5.2.2`; the configured feed's latest
  Teams SDK 2.1.1 still declares `^5.6.0`, so upgrading to that release alone
  does not fix the advisory.
- The SDK's token manager uses `ConfidentialClientApplication`,
  `acquireTokenByClientCredential`, `ManagedIdentityApplication.acquireToken`
  and `LogLevel`. The gateway requires client-secret credentials, exercising
  the confidential-client path. It does not use interactive loopback login.
- [MSAL's v6/v7 breaking changes](https://github.com/AzureAD/microsoft-authentication-library-for-js/blob/msal-node-v7.1.0/lib/msal-node/CHANGELOG.md)
  remove interactive loopback/response-mode options, not those SDK APIs.
  Node >=20 remains supported; this gateway requires Node >=22.
- [Teams upstream adopted MSAL 7](https://github.com/microsoft/teams.ts/pull/911)
  and [subsequently adopted 7.1.1](https://github.com/microsoft/teams.ts/commit/528c8e9c24ef6b140acf07c7d3ece58abc72966d).
  This is a locally verified integration pin outside the published SDK's semver
  range, not a claim that SDK 2.0.15 officially supports MSAL 7.

Remove the scoped override when adopting a released Teams SDK that declares a
patched MSAL range (minimum 7.1.0), then update the resolution assertions and
rerun the tests below. Keep canonical public registry URLs and SHA512 integrity
for both upgraded MSAL packages; obtain packages through the configured feed
when public registry access is unavailable.

## Focused validation

Run from `bridge/teams-gateway` with `NODE_OPTIONS=--max-old-space-size=1536`:

```sh
npm test -- tests/msal-compatibility.test.ts tests/gateway.test.ts tests/kubernetes.test.ts --maxWorkers=1
npm run typecheck
./node_modules/.bin/tsc --noEmit --target ES2024 --module NodeNext --moduleResolution NodeNext --strict --skipLibCheck tests/msal-compatibility.test.ts
npm run lint
```

The compatibility tests execute the installed SDK and real MSAL acquisition,
cache and error paths with a test-only network transport. They check bot/Graph
scopes, tenant selection, credential errors, authorization-code host injection,
state and nonce validation, and instance-aware rejection. Inbound tests exercise
the real SDK JWT validator against a loopback JWKS endpoint and locally signed
RS256 tokens, including audience, issuer, expiry, service URL and signature
failures. No production auth behavior or trust configuration is changed.

These are local regression tests, **not real Entra/Teams acceptance**. Live
credential exchange, inbound Teams delivery and outbound delivery still require
an authorized deployment acceptance run. An unavailable or rate-limited npm
audit is not a passing audit; the exact advisory and locked version establish
remediation of this advisory only.
