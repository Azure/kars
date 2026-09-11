# Connections

The **Connections** tab is where a signed-in user connects services for work
they create. GitHub connections are principal-scoped; channels remain
workspace-scoped. No agent ever handles a raw credential.

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

Wire **Telegram, Slack, Discord, WhatsApp, or Microsoft Teams** once for the workspace. Any
mission or team can then report progress and deliverables over them — regardless
of harness.

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
  never echoes it back. `GET` only reveals which channels are *enabled*.
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
