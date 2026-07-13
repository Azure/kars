# Keyless Git Write — the agent git gateway

kars lets an agent open, review, and merge GitHub pull requests **without ever
holding a credential**. The agent uses ordinary `git` and `https://github.com/…`
URLs; the router injects a short-lived, repo-scoped token at a loopback proxy, and
the controller wires everything up per sandbox. This is the same posture as the
inference path — the agent never sees a secret, and the boundary is enforced in
Rust, not in the agent.

## Where the grant lives

Git write is a **sub-block of the sandbox's provenance**, not its own CRD. The
Bridge (or an operator) declares which repos a mission/team may write, via an
annotation on the `KarsTask` / `KarsTeam`:

```yaml
spec:
  blueprint:
    gitWrite:
      connectionConfigMapRef:
        name: kars-github-connection-0123456789abcdef
      repos: ["owner/repo-a", "owner/repo-b"]
```

The controller clamps the declared set to the repos the authenticated
principal's GitHub connection actually granted (`declared ∩ connection`) and materializes a
per-sandbox `<name>-git-write` secret carrying the installation id, the clamped
repo scope, the git role, and the author identity — **never** the App private key.

## How it works

```
agent  ──git push / curl github.com──▶  loopback reverse-proxy (router :8443)
                                          │  /git/*     → github.com   (git http)
                                          │  /gh-api/*  → api.github.com (REST)
                                          ▼
                              mint short-lived, repo-scoped
                              GitHub App installation token
                              (never exposed to the agent)
```

- **Transparent URLs.** The controller mounts a system `/etc/gitconfig` with
  `insteadOf` / `pushInsteadOf` rewrites so every `https://github.com/…` and
  `git@github.com:…` URL is routed to `http://127.0.0.1:8443/git/…`. This lives in
  `/etc/gitconfig` (not `$HOME`) so it applies regardless of the tool shell's
  `HOME`/env — a plain `git push` "just works".
- **Token injection.** The router (`inference-router/src/routes/github_proxy.rs`)
  is loopback-source-only. It mints a repo-scoped GitHub App installation token
  and injects it on the way out; out-of-scope repos get a `403`.
- **No agent-minting path.** The old `/v1/github-token` endpoint returns `410
  Gone`; there is no way for the agent to obtain a write credential itself.

## Key custody (multi-tenant)

- The GitHub App **private key** lives in exactly one secret (`kars-github-app`,
  `kars-system`), mirrored only into each git-write sandbox namespace and mounted
  **only to the router container** — never the agent.
- Each principal connects their **own** installation/repo set. Bridge stores it
  in a ConfigMap named `kars-github-connection-<subject-hash>` containing the
  installation id, account, and reachable repos (no key). A mission's write
  scope can never exceed its creator's connection.

## Sub-agent attenuation & mandatory review

- **Role.** The git-write secret stamps `KARS_GIT_ROLE` (`principal` |
  `subagent`); the controller derives `subagent` from the `kars.azure.com/parent`
  label. Only a principal may merge (`GitWriteConfig::can_merge`).
- **Merge is review-gated.** The router refuses a merge (`PUT …/pulls/{n}/merge`)
  until a review has been submitted and the latest decisive review is not
  `CHANGES_REQUESTED`. Sub-agents cannot submit reviews, so a sub-agent's PR can
  only be merged after a **principal** reviews it. (Because every agent acts under
  one shared App identity, GitHub forbids approving your own PR — so a `COMMENT`
  review satisfies the gate; an `APPROVED`-only gate would deadlock.)

## Team runs

A `KarsTeam` with `spec.blueprint.gitWrite` preserves the grant on **every** run
it mints (principal, merger, task-force), so a standing team — and the
sub-agents its principal spawns — can open PRs. See
`controller/src/kars_team_reconciler.rs::apply_task`.

## Controller & router surface (what changed)

| Component | Change |
|---|---|
| `inference-router/src/git_write.rs` | `GitWriteConfig` (App/PAT + fail-closed repo allowlist + `GitRole`); `repo_allowed`, `token`, `can_merge`. |
| `inference-router/src/routes/github_proxy.rs` | Loopback `/git/*` + `/gh-api/*` proxy; token injection; repo-scope 403; merge + mandatory-review gate (`review_states_permit_merge`). |
| `inference-router/src/routes/github_token.rs` | `/v1/github-token` → `410 Gone` (agent can't self-mint). |
| `controller/src/reconciler/mod.rs` | Read the typed principal ConfigMap reference, materialize `<name>-git-write` (clamped to `declared ∩ connection`), mount `/etc/gitconfig`, and mirror the App secret to the router only. Legacy annotation + fixed Secret remains read-only fallback. |
| `controller/src/kars_team_reconciler.rs` | Propagate the team's git-write grant onto every run. |

## Deliverable

A pull request is a first-class **delivery type**: the Bridge extracts opened PRs
from the run output and surfaces them as artifacts (repo + number + link), so a PR
is tracked and reviewable alongside files and reports.
