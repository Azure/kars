<!-- Copyright (c) Microsoft Corporation.
Licensed under the MIT License. -->

# Connections

The **Connections** tab is where a signed-in user connects services for work
they create. GitHub connections are principal-scoped; channels remain
workspace-scoped. No agent ever handles a raw credential.

## Shared GitHub App setup

In **Console → Configuration → GitHub App**, choose one of two setup paths:

- **Use an existing GitHub App:** an operator supplies its numeric App ID and
  uploads the GitHub-generated `.pem` file (unencrypted RSA, up to 16 KiB).
  There is no private-key textarea. The server parses the file, verifies it
  against GitHub and writes it through the existing enrolled credential store.
- **Register a new GitHub App:** an administrator signed in through SSO chooses
  a personal or organization owner, a name and private/public installation
  visibility. Private means only the owner can install it. Optional read
  permissions for engineering security signals are off by default. Choose
  **Prepare GitHub registration**, then **Continue to GitHub** to review and
  create the App. You own the App; Kars does not create it until you approve on
  GitHub. Webhook delivery and installation-time OAuth are disabled.

Both paths require enrolled, ready `kars-github-app` storage. Use HTTPS, or a
localhost connection for local development. Upload does not make the key
unnecessary or inaccessible to authorized cluster administrators: it keeps the
key out of visible form values and responses, while preserving the existing
private-store controls. Agents do not receive the App signing key.

Guided registration returns to the same Bridge origin and browser session.
Complete it within GitHub’s one-hour deadline; starting another registration
replaces the previous transient flow. Kars validates signed, session-bound state
before the BFF exchanges the temporary code with GitHub. Generated credentials
remain server-side, and only the App ID and signing key are stored. The BFF
checks store identity/version before saving; it cannot silently replace a
configured App or newer store state. The web server gains no GitHub egress.

If GitHub creates the App but callback verification, conversion or storage fails,
do not reuse the callback URL: the code may already be consumed. Keep the App,
repair storage if needed, generate a fresh PEM in its GitHub settings and use
**Use an existing GitHub App**. An existing configuration must be explicitly
**Disconnected** before replacing it through the Console.

App configuration is **not** repository authorization. After setup, installation
owners still choose repositories and approve permissions through **Workspace →
Connections**. App verification alone does not prove effective installation
permissions or successful repository operations.

## GitHub (keyless pull requests)

Install the kars GitHub App on the repositories you want your agents to work on,
then **Connect**. The admin-configured App is shared, but every authenticated
principal gets an isolated ConfigMap named from a SHA-256 hash of their immutable
subject. It stores only the installation id, account, and reachable repos.

When a mission or team needs to push, the router mints a short-lived,
repo-scoped GitHub App token and injects it at a loopback proxy; the agent uses
ordinary `git` and `github.com` URLs and never sees a token. Disconnect revokes
instantly. Granting write access to a mission/team is done from the composer's
**Pull request access** control. The BFF rejects any requested repo outside the
signed-in principal's grant and derives the connection reference server-side.

For typed Missions, the reviewed execution plan also bounds this grant. Without
`github-pr` in any phase or synthesis capability list, the picker displays
**Read-only repository access** and the BFF creates a `githubBinding.write: false`
binding without `gitWrite`. The router denies GitHub mutations even when a native
HTTP tool can make non-GET requests to other services. A plan with `github-pr`
keeps the explicit pull-request grant. Legacy Missions and the existing Team
pull-request control retain their prior behavior. The API's legacy
`git_write_repos` field carries the selected repository set in both modes;
clients cannot override the mode inferred server-side from the reviewed plan.

Keyless setup binds an empty, UID-owned credential source; it does not store a
GitHub token. Controller status and initial cleanup-finalizer conflicts are
retried with the same reviewed intent and fresh resource-version fences. Secret
value writes and explicit credential reviews retain their no-automatic-retry
behavior. If setup already saved a never-launched draft and acknowledged its
empty source, **Launch** can finish that missing binding on the same draft before
opening the execution gate. This recovery cannot create or rewrite a source,
adopt another target's source, widen repository access, or resume a used Mission.

Engineering intake can also read Dependabot vulnerability, code scanning, and
secret scanning alerts. Configure the shared GitHub App with read access to all
three alert families in addition to Metadata, Checks, and Commit statuses.
Existing installations must approve newly added permissions. A repository must
also have the corresponding GitHub security product enabled. Bridge reports
missing permission/feature access as partial or unavailable; it never treats a
403/404 as a successful scan with zero findings. Secret values are never stored
in backlog tasks or status.

Full mechanics are documented in `docs/git-write.md` in the compatible Kars
checkout. The page is not yet available on the public Kars `main` branch.

## Channels (agent-agnostic)

Configure **Telegram, Slack, Discord, WhatsApp, or Microsoft Teams** for the workspace.
Reporting support differs by channel. **Configured** in the workspace and Team channel
cards means settings are saved, not that delivery has been verified. Team qualification
is shown separately.

WhatsApp uses an **Enable WhatsApp** checkbox, not a token field. Saving sends the
existing `"true"` enable flag; it does not complete or verify device pairing. The
Bridge form does not provide a pairing QR code.

> **Proactive vs inbound.** Today an agent can **proactively** push status/
> deliverable updates over **Telegram** (the `telegram_status` tool). Slack,
> Discord, and WhatsApp are wired as **inbound conversational** channels (the
> agent replies to messages you send it), not proactive push. Telegram proactive
> reporting also requires the **allowed chat IDs** (`TELEGRAM_ALLOW_FROM`) — set
> them when connecting, or the agent has no one to send to.

Microsoft Teams uses a dedicated gateway rather than exposing bot credentials
to agent sandboxes. It supports inbound team commands, proactive progress and
approval cards, and distinct Approve / Request changes / Deny decisions.

- **Write-only tokens.** The token is typed into a password field and written
  straight into the `kars-workspace-channels` secret (`kars-system`); the API
  never echoes it back. `GET` only reveals which channels are *enabled*. Kubernetes
  Secret encryption at rest depends on the cluster's configuration; saving a Secret
  alone does not establish it.
- **Agent-agnostic propagation.** The controller copies `kars-workspace-channels`
  into **every** run sandbox's `<sandbox>-credentials` secret (mounted via
  `envFrom optional`) before the pod starts, so any agent's entrypoint wires up
  the channel from it. A standing team may still layer its own
  `kars-team-channel-<team>` secret on top (team keys win).
- **Teams isolation.** Teams client credentials and the Entra-to-Bridge identity
  map live only in `kars-bridge-teams`; they are mounted by the gateway and BFF,
  never copied into sandbox credentials. An admin maps each Teams Entra object
  ID to that person's immutable Bridge OIDC subject.

| Channel | Env key(s) the entrypoint reads |
|---------|---------------------------------|
| Telegram | `TELEGRAM_BOT_TOKEN`, `TELEGRAM_ALLOW_FROM` |
| Slack | `SLACK_BOT_TOKEN` |
| Discord | `DISCORD_BOT_TOKEN` |
| WhatsApp | `WHATSAPP_ENABLED` |
| Microsoft Teams | Dedicated gateway secret; no agent-visible credential |

### Microsoft Teams prerequisites

1. Entra App Registration and Azure Bot resource for the tenant.
2. Bot installed in the target Teams chat/channel.
3. TLS ingress for `/api/messages`.
4. Admin-provided identity map:
   `[{"entra_subject":"<oid>","bridge_subject":"<oidc-sub>","roles":["operator"],"name":"Alice"}]`.

### API

| Method | Path |
|--------|------|
| GET | `/api/namespaces/{ns}/channels` — which channels are enabled |
| POST | `/api/namespaces/{ns}/channels` — enable/update a channel (write-only token) |
| DELETE | `/api/namespaces/{ns}/channels/{channel}` — disable a channel |
