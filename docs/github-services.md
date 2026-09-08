# Optional keyless GitHub engineering services

The router can authenticate a bounded set of GitHub repository operations with
a GitHub App. This is independent of Bridge, inference-provider credentials,
`credentialsRef`, and the existing CLI credential flags. With no configuration,
the new service returns 404 and existing standalone behavior is unchanged.

**Deployment blocker:** the reviewed SRE Secret-access/authority repair and its
GitHub issuance/reuse integration must be present. The historical SRE agent's
cluster-wide Secret-read permission would defeat router-private custody.
The required controller gate is
`crate::sre_authority::privacy_epoch(client, target_namespace)`: actual shared
GET/LIST/WATCH denials and current v2 Ready/retired registration proof, not merely
Sandbox/task identity or an authorization digest. That gate is not wired into
this baseline's GitHub service. Merging prerequisite ancestry alone does not
complete the integration; issuance/reuse must fail closed on missing or stale
privacy proof and be independently qualified. Do not provision or enable the
GitHub App Secret until that integration is complete.
See the [pending security audit](security-audits/2026-09-08-github-services.md).

## Operator configuration and removal

The controller mounts an optional Secret named **`router-github-app`** in the
owned sandbox namespace, at `/etc/kars/github`, **only in the inference-router
container**. It never creates or copies GitHub credentials. Provision this Secret
only after the privacy-gate prerequisite above is complete, using the operator's
existing secret-management workflow, not an agent tool.
The Secret has one key, `config.json`, containing:

| JSON field | Requirement |
| --- | --- |
| `identity` | Exact JSON object from that router's `KARS_SERVICE_IDENTITY_JSON`, including Sandbox namespace/name/UID, namespace UID, managed flag, and any task UID/generation/full authorization digest |
| `app_id` | Positive numeric GitHub App ID, as a string |
| `installation_id` | Positive numeric installation ID |
| `private_key_pem` | RSA private key PEM issued for that App, as a JSON string |
| `repositories` | Explicit nonempty list of full `owner/repo` names; maximum 32 |
| `write` | Optional boolean, **false by default** |

Unknown fields, malformed identities, missing scope, wrong namespaces or
incarnations, and mismatched task authorizations fail closed. Configuration is
bounded to 64 KiB. There is no host override, PAT fallback, process-wide GitHub
token fallback, or agent-visible token exchange. Do not place this configuration
in `<name>-credentials`, shared inference-provider Secrets, agent environment
variables, or CLI provider credential sources.

Use a GitHub App installed only on the intended repositories. Read mode requests
`actions`, `checks`, `contents`, `issues`, `metadata`, `pull_requests`, and
`statuses` read permission.
Write mode requests write for contents/issues/pull_requests only, while
actions/checks/statuses/metadata remain read. The service refuses broader or
missing returned
permissions and verifies both the repository's installation/App identity and the
token's full owner/repository provenance before caching it.

Previously enrolled installations must approve the additional **Checks: read**
and **Commit statuses: read** permissions. Both are required by the exposed
check-run and combined-status reads, including when writes are enabled. An
installation that lacks a required grant fails token acquisition honestly; the
router never retries with fewer permissions or falls back to another credential.
Changing App permissions alone does not repair an older router's token profile.

Update the single `config.json` key atomically to rotate identity, key,
installation, permissions or scope. The next request observing the projected
change discards the entire old cache. Invalid replacement never reuses the old
credential. Removing the Secret or key disables new requests once Kubernetes has
propagated removal; Kubernetes Secret projection is eventually consistent, not
instant revocation. For immediate credential revocation use GitHub App
installation/token controls as well. An already-dispatched mutation may finish.
Recreated Sandbox/namespace/task identities require explicit operator enrollment;
credentials are not inherited or automatically provisioned for spawned workers.

## Agent-facing contract

Requests are accepted only from a loopback peer in the same pod. No caller
credential, cookie, proxy header, or redirect authorization is forwarded.

| Endpoint | Behavior |
| --- | --- |
| `GET /v1/github/status` | Keyless enabled/write booleans only; 404 when absent, 503 for invalid configuration |
| `/git/{owner}/{repo}.git/info/refs?service=git-upload-pack` | Git smart-HTTP discovery |
| `POST /git/{owner}/{repo}.git/git-upload-pack` | Clone/fetch |
| Corresponding `git-receive-pack` discovery/POST | Push, only with explicit `write: true` |
| `/gh-api/repos/{owner}/{repo}/…` | Bounded REST reads and explicitly enabled issue/PR creation/comments |
| `GET /gh-api/repos/{owner}/{repo}/actions/jobs/{job_id}/logs` | Actual Actions job log bytes, downloaded by the router |
| `/v1/github-token` | Always 410; raw credentials are never returned |

For example, an agent can clone using
`git clone http://127.0.0.1:8443/git/OWNER/REPO.git` or call the REST prefix with
an ordinary HTTP client. No `gh auth login`, credential helper, token response,
entrypoint rewrite, or changes to existing CLI flags are required. Normal HTTPS
GitHub URLs are **not silently rewritten**.

Supported REST reads cover repository metadata, branches/tags, commits/checks,
issues/comments, pull requests/files/commits/reviews, and Actions
runs/workflows/jobs/job logs. Bounded `page`/`per_page` and listed filter queries
are accepted; credential query parameters and duplicate parameters are denied.
In write mode only issue/PR creation and issue comments are exposed. PR creation
requires a same-repository head (no `owner:branch` cross-repository head).
Repository transfers/forks, administration/secrets, GraphQL, arbitrary content
URLs, release upload/download, workflow dispatch/cancel/rerun, reviews, and merges
are not exposed. Encoded paths, dot segments, user-selected hosts, and arbitrary
redirects are rejected before dispatch.

**Write authority is repository-wide, not branch-wide.** Git smart-HTTP packfiles
are not a router-level branch authorization mechanism. Before enabling write,
an operator must configure GitHub rulesets/protected branches that prevent the
App from bypassing protected/default branches and workflow-file restrictions.
If those controls cannot be established, leave `write` false. This service does
not implement independent review, branch ownership, workflow scheduling, or
automated publication/merge policy.

## Egress, bounds, errors, and logs

- Existing signed egress policy and threat blocklist remain mandatory for both
  GitHub API authentication and data-plane hosts. No implicit allowlist grant is
  introduced. Git/API destinations are fixed to `github.com`/`api.github.com`.
- Actions logs accept one GitHub 302 to HTTPS port 443 under
  `.blob.core.windows.net` or `.actions.githubusercontent.com`, without userinfo
  or fragments. The exact destination must also pass existing egress policy.
  The download sends **no GitHub credential** and never follows another redirect.
  Prefer exact operational storage-host egress approvals over entire suffixes.
- Two concurrent requests per router; 90-second total deadline; each upstream
  operation has a 10-second connect and 45-second request timeout. No automatic
  retries, including 401 or ambiguous accepted mutation failures.
- Git requests/responses: 16 MiB each. REST requests/responses: 2 MiB each.
  Job logs: at most 32 MiB downloaded; last 2 MiB returned with
  `X-Kars-Log-Truncated: true` when truncated. Oversized or interrupted upstream
  responses fail explicitly, rather than fabricating successful partial logs.
- Git POST accepts an absent content encoding or one `Content-Encoding: gzip`
  value (case-insensitive, emitted canonically as `gzip`). Compressed negotiation
  bytes are forwarded unchanged with that validated coding; the 16 MiB request
  cap applies to **wire bytes**, without decompression in the router.
  Unsupported, comma-separated or repeated encodings return 415 before token
  acquisition. Content encoding on other methods/API requests is unsupported.
  No other client headers are implicitly forwarded.
- GitHub authentication error bodies, signed URLs, tokens and request bodies are
  never logged by the service. Upstream non-success responses preserve actionable
  HTTP status but suppress bodies and redirect/cookie headers. Responses are
  `Cache-Control: no-store`; successful GitHub/log data is untrusted content.
  The OpenClaw wrapper does not checkpoint CI log tool results into its activity
  memory buffer.
- Tokens are cached per repository inside an immutable credential incarnation.
  Concurrent misses share one mint; expiration is refreshed with a 60-second
  margin. A 401 invalidates that exact cached token for a **future** request,
  never replays the current request.

## OpenClaw tool and downstream prerequisites

`github_actions_job_logs(owner, repo, job_id, tail_lines?)` is independently
registered through the existing governed tool wrapper. It uses the loopback
keyless endpoint, validates path/job inputs, has a 95-second deadline and 2 MiB
response cap, and returns JSON with `repository`, `job_id`, `http_status`,
`tail_lines`, `truncated_before_tail`, and `log`. Defaults: 250 final lines;
maximum: 2,000. HTTP failures are explicit tool errors, not synthetic logs.

Bridge/BFF or future runtime plans may depend on this exact contract, but must
first enroll the current Sandbox identity, configure the App/repositories, and
approve the required egress destinations. HTTP 404/403/503 is a missing or invalid
prerequisite, not permission to acquire a fallback token. Task/worker scheduling,
service enrollment automation, MCP/memory, durable budgets, SRE redesign, and
Bridge UI are separate layers. This change does not enable them or widen any
existing task launch contract.
