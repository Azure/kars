<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

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

Previously enrolled installations must approve every required permission, including
**Actions: read**, **Checks: read**, **Commit statuses: read**, and **Issues: read/write**
when the binding enables writes. Earlier Console setup instructions omitted Actions
and Issues. A write-enabled binding requests the write token profile even for a
read-only task; missing any required permission can therefore block all of its
GitHub requests. Successful App credential verification and repository discovery
alone do not verify the router's token permission profile. An
installation that lacks a required grant fails token acquisition honestly; the
router never retries with fewer permissions or falls back to another credential.
Changing App permissions alone does not repair an older router's token profile.

An authentication failure returns the same generic 502 to the agent. Router logs
identify the failing stage (`jwt`, `installation_lookup`, `installation_validation`,
`token_mint`, or `token_validation`), a fixed reason, and an HTTP status when available.
They never include credentials, upstream bodies, repository names, or request URLs.
Use that stage to distinguish connection/HTTP failures from rejected identity,
permission, repository, expiry, or token-format provenance; do not assume a 502 means
missing installation permissions.

Installation tokens are opaque: GitHub's stateless `ghs_APPID_JWT` format includes
periods and variable-length content. The router does not decode or validate that
JWT. It retains its 16–4096-byte transport bound and RFC 6750 bearer syntax checks
(including rejection of whitespace/control characters), alongside the unchanged
installation, repository, exact permission, and expiry checks on GitHub's response.

Update the single `config.json` key atomically to rotate identity, key,
installation, permissions or scope. The next request observing the projected
change discards the entire old cache. Invalid replacement never reuses the old
credential. Removing the Secret or key disables new requests once Kubernetes has
propagated removal; Kubernetes Secret projection is eventually consistent, not
instant revocation. For immediate credential revocation use GitHub App
installation/token controls as well. An already-dispatched mutation may finish.
Recreated Sandbox/namespace/task identities require explicit operator enrollment;
credentials are not inherited or automatically provisioned for spawned workers.

## Bridge repository readiness

Before creating a Mission or Team with selected repositories, run **Check readiness**.
It checks both your connected repository list and the workspace administrator's current
approval. Connecting GitHub does not automatically approve repositories for a workspace.
If approval fails, open **Configuration → GitHub** to check the connected repositories;
if they are connected, ask your workspace administrator to approve the selected
repositories and the required read-only or pull-request permission, then check again.
Typed Missions derive that permission from the reviewed `github-pr` capability;
Teams retain their existing pull-request permission requirement.

Protected repository access cannot be combined with direct GitHub entries in the
package's **Network** settings or Team role settings. Readiness reports those entries
(including wildcard matches); remove the reported entries, keep the repositories
selected, and check again. Nothing is silently removed. This does not remove protected
GitHub access or require you to add a token.

Changing the repository selection invalidates the previous check. Review and submission
use the same selection, including after returning to composition. Creation still checks
live authority again. A saved Mission's readiness check uses its stored binding and
current approval; a replaced connection or approval may require a new draft. Readiness
is not proof of a successful GitHub operation or completed customer acceptance.

## Agent-facing contract

Requests are accepted only from a loopback peer in the same pod. No caller
credential, cookie, proxy header, or redirect authorization is forwarded.
Successful REST responses are validated as bounded JSON and GitHub's
`temp_clone_token` field is removed from every object, including repositories
nested in pull requests or arrays, before either native runtime or the REST
adapter receives it. Other metadata is preserved; malformed JSON fails closed.
Git smart-HTTP bytes and Actions log handling remain separate and unchanged.

Native `http_fetch` results include `observed_at`, an RFC 3339 UTC timestamp from
the router clock after reading the bounded response body. Both runtimes preserve
this field. It is the router's observation time, not GitHub's `Date` header, a
commit timestamp, or proof that sequential reads are atomic. For truncated
responses it records when the bounded read stopped, not complete receipt.

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
entrypoint rewrite, or changes to existing CLI flags are required. Native reviewed
execution does not need (and must not enable) local URLs or shell access: both
Hermes and OpenClaw use `POST /egress/fetch` with a canonical
`https://api.github.com/repos/OWNER/REPO/…` GET. When configured, the router
dispatches that request through the same keyless GitHub service, sharing its
repository policy, token cache, concurrency limit and socket-derived same-pod
check. Only GET without a body or caller credentials is accepted on this path;
optional `Accept` and `User-Agent` string headers are accepted but discarded in
favor of the service's fixed GitHub media type and router user agent. Other caller
headers, including credentials, remain rejected before token acquisition.
Noncanonical origins, encoded/dot paths, unsupported queries and repositories
outside the binding fail closed. Ordinary native network/file permissions remain
read-only with respect to GitHub. Publication requires the explicit native
`github-pr` capability described below.

The fetch endpoint retains its authentication, scoped egress dispatch claim and
telemetry, and returns the usual `status`, `headers`, `body`, `truncated`
observation (including log-tail truncation). GitHub fetch has a 30-second total
deadline. Invalid configuration or removal after a configuration was observed
(including the projection at router startup) never falls back to unauthenticated
GitHub fetch. Governed bindings use a required projection; missing initial
configuration prevents the sandbox from starting. A router that has never had
GitHub configuration retains ordinary public fetch behavior; other public HTTPS
hosts remain ordinary egress, with no GitHub credential injection. General HTTPS
traffic and Git clone URLs are not rewritten.

For Team native GET acceptance, use the member's reviewed `githubBinding`, not
raw `api.github.com:443` or `github.com:443` egress entries. The controller rejects
those opaque permissions with a keyless binding. The live identity-specific
projection authorizes only repository-enforced REST/Git service requests;
strict-mode public fetch and opaque proxy permissions are not enlarged. Threat
intelligence denials and the inference-budget model/catalog fence still apply.
A missing role `githubBinding` inherits the Team blueprint binding; controller
task execution propagates it to the member sandbox and provisions the private
identity-specific projection. A Team connection marked Ready alone does not
prove that a member's projection is ready. Tell native roles to use canonical
`https://api.github.com/repos/OWNER/REPO/…` GETs, not the GitHub website or an MCP
catalog lookup. Job-log redirects separately require the exact signed download
host to pass ordinary egress policy; explicit role blueprints do not inherit
the Team's egress list.

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

## Reviewed native PR publication

Both native runtimes expose the same bounded transaction engine: OpenClaw
`kars_mission_github_pr` and Hermes `github_pr`. A reviewed native member phase,
or a single-agent Mission's native synthesis, must explicitly grant `github-pr`.
A write-enabled GitHub binding, `network`, or filesystem access alone never
creates that authority. Decomposed synthesis cannot publish; the authorized
member retains the actual observed PR URL/commit for file-only synthesis. The
normal reviewed plan, unique model invocation, router tool policy, same-pod
service checks and outcome receipt all remain required. Existing installed route
qualification must cover this capability; the adapter is not qualification evidence.

1. `inspect` requires `operation`, canonical `repository` (`owner/repo`),
   and repository-relative `path`. Omit the create-only fields (`expected_base`,
   `content`, `title`, `body`) or set them to `null` when a provider requires every
   schema property. Empty strings and other non-null create values are rejected
   during inspection. It reads the default branch and returns `expected_base`,
   the existing UTF-8 content, or `exists=false` for a new file.
2. `create` additionally requires that exact `expected_base`, full replacement
   `content`, `title`, and `body`. A changed base or unchanged content fails before
   publication. Bridge retains producer and usage evidence for the explicitly
   reviewed native `github-pr` capability, including runs with failed tool calls;
   a valid producer record alone does not establish successful PR publication.
   Only one regular text file changes; existing modes and all other
   tree entries are preserved. File contents are limited to 128 KiB, titles to
   256 UTF-8 bytes and bodies to 64 KiB.
3. The adapter uses fixed Git argv (never shell), a private bare object repository,
   and the existing fixed loopback keyless `/git/OWNER/REPO.git` transport. It
   never checks out or executes repository content. Hooks, credential helpers,
   inherited Git configuration/proxies, remote protocols other than fixed HTTP,
   submodule recursion, signing and attributes processing are disabled. Paths
   beginning with `.git` in any segment (including `.github`, `.gitmodules` and
   `.gitattributes`), traversal, symlinks, gitlinks, caller branches/refspecs and
   arbitrary configuration are rejected. New-file parent replacement is rejected.
4. A generated `kars/reviewed/<random-id>` branch is published with Git's atomic
   empty-expected-old lease. This can **only create an absent ref**, not overwrite
   an existing ref—even an unprotected ref. Default-branch writes, force updates,
   merges and deletions are unavailable. Server branch/ruleset restrictions still
   apply; there is no bypass request. The adapter POSTs the same-repository PR
   through `/gh-api`, and verifies the actual 201 response, URL, branch, commit,
   and base/head repositories before reporting success. Local exports are not PRs.

A private, fsynced, create-exclusive publication receipt under
`/sandbox/.kars-github/<assignment-identity-hash>/` is claimed **before** push.
Ordinary native file tools cannot access this directory. It prevents a second
publication for that repository/assignment, even with a new model call ID or
runtime helper instance. Every post-dispatch error is conservatively ambiguous:
retain its branch/commit/stage, inspect upstream state through approved read-only
flows, and reconcile manually; never automatically retry or claim rollback.
A push may have succeeded even if the PR request or response failed. Runtime
outcome receipt failures likewise halt execution rather than replaying. The
publication receipt survives object-workspace cleanup and cancellation; abrupt
host/pod destruction may lose local receipts, so absence is not proof of no effect.
The runtime reports harness-observed tool evidence, not a new router dispatch
attestation or fabricated success URL.

The Git subprocess has fixed operation deadlines, output/pack-file/process
resource bounds, and an isolated process group terminated on cancellation.
Linux helpers and their Git children bind their lifetime to the parent identity
captured before spawn, including a legitimate namespace PID 1; reparenting still
fails closed.
These are host safety limits,
not inference/tool-spending allowances. Normal completion removes object
workspaces while retaining publication receipts; after abrupt process loss,
operators may remove abandoned object directories but must preserve receipts.

The shared engine is maintained in the Hermes package and copied, dereferenced,
into OpenClaw's built distribution; both runtime images must package it alongside
Git and Python. Update plan/CRD schemas and composer alongside runtime packaging.
No router/token implementation is replaced by this adapter.

**The generic router write prerequisite above is not waived.** This narrower
native adapter independently prevents workflow edits and existing/default-ref
updates, but it does not turn the general Git smart-HTTP endpoint into branch-
scoped authorization, nor establish protection settings for other consumers.

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
