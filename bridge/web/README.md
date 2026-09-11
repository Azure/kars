# Kars Bridge web

The Next.js application provides three persona-scoped products:

- `/workspace` for employees;
- `/console` for operators and administrators;
- `/audit` for auditors.

The browser calls same-origin `/api/*` routes. The Next.js server proxies those
requests to the Rust BFF using `BRIDGE_BFF_URL`; browser code never receives a
Kubernetes credential.

## Development

```bash
npm ci
npm run dev
```

Required integration configuration:

| Variable | Purpose |
|---|---|
| `BRIDGE_BFF_URL` | Server-side BFF origin |
| `BRIDGE_OIDC_ISSUER` | OIDC issuer |
| `BRIDGE_OIDC_CLIENT_ID` | OIDC client |
| `BRIDGE_OIDC_CLIENT_SECRET` | OIDC client secret |
| `BRIDGE_SESSION_SECRET` | Signs Bridge sessions |

Use repository-level `make dev` to run the BFF and web application together.

## Build check

```bash
npm run build
```

`npm run lint` is configured but currently reports a known private-preview
React-rule backlog. It is not a green release gate yet.

Do not restore the create-next-app boilerplate or deploy this application
directly to Vercel. It is designed to run next to the BFF and a Kars cluster.
