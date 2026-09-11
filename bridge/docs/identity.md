# Identity and sign-in

Bridge supports standards-based OIDC and an optional in-cluster Dex deployment.

## OIDC flow

1. The browser starts Authorization Code + PKCE.
2. The provider authenticates the user.
3. Bridge verifies issuer, audience, signature, nonce, and callback state.
4. Provider groups/roles map to Bridge roles.
5. Bridge issues a signed session cookie.
6. The web layer propagates a signed principal assertion to the BFF.
7. The BFF independently verifies that assertion before authorizing routes.

## Required configuration

| Variable | Purpose |
|---|---|
| `BRIDGE_OIDC_ISSUER` | Provider issuer URL |
| `BRIDGE_OIDC_CLIENT_ID` | OIDC client |
| `BRIDGE_OIDC_CLIENT_SECRET` | Confidential-client secret |
| `BRIDGE_SESSION_SECRET` | Signs Bridge sessions |
| `BRIDGE_PRINCIPAL_SECRET` | BFF verification key; must equal the web session secret |
| `BRIDGE_OIDC_ROLE_CLAIM` | Claim containing groups/roles |
| `BRIDGE_OIDC_ROLE_MAP` | Provider claim to Bridge role mapping |

Use Kubernetes Secrets or an external-secret controller. Never place secrets in
Helm values committed to source control.

For external OIDC, create one shared signing Secret and wire it to both
components:

```yaml
auth:
  principalSecretName: kars-bridge-principal
  principalSecretKey: session-secret

web:
  extraEnv:
    - name: BRIDGE_OIDC_ISSUER
      value: https://id.example.com
    - name: BRIDGE_OIDC_CLIENT_ID
      value: kars-bridge
    - name: BRIDGE_OIDC_CLIENT_SECRET
      valueFrom:
        secretKeyRef:
          name: kars-bridge-oidc-client
          key: client-secret
```

The chart injects `BRIDGE_SESSION_SECRET` into web and
`BRIDGE_PRINCIPAL_SECRET` into the BFF from `auth.principalSecretName`.
Without the BFF secret, the BFF intentionally falls back to local-development
authorization and the deployment is not a valid multi-user configuration.

## Dex private preview

The chart can deploy Dex with static password users. The web proxies `/dex/*`
so a single port-forward is enough:

```bash
kubectl -n kars-system port-forward svc/kars-bridge-web 3000:3000
```

The default issuer is `http://localhost:3000/dex`. This split-horizon design is
for colleague testing over port-forward. In-cluster browsers cannot follow that
localhost redirect because localhost refers to the browser pod itself.

For ingress, set an HTTPS issuer and callback URI reachable by users and Dex.

## Sessions and logout

- Session cookies are signed.
- Secure cookies are used when the forwarded scheme is HTTPS.
- Logout uses a non-preserving redirect so the browser does not repeat the POST.
- Unauthenticated protected routes redirect to login; they do not fall back to
  a privileged local persona.

## Production checklist

- Replace seed passwords.
- Configure real groups and fail-closed role mapping.
- Rotate client and session secrets.
- Set HTTPS issuer and redirect URIs.
- Define session lifetime and emergency revocation.
- Test each persona using direct deep links, not only navigation menus.
- Test direct BFF API denial: an auditor token must receive 403 for
  `/api/namespaces/<ns>/tasks`.
