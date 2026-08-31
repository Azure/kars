# Feishu Channel Contract Design

**Status:** Approved design

**Date:** 2026-08-09

**Scope:** A typed, runtime-aware channel contract with first-class Feishu support for OpenClaw and Hermes. Feishu uses outbound WebSocket long connections only. Other runtimes must report unsupported capability rather than silently ignore channel configuration.

## 1. Problem

Kars currently exposes four messaging channels through CLI flags and per-sandbox credentials: Telegram, Slack, Discord, and WhatsApp. The platform path is:

```text
CLI flag -> <sandbox>-credentials Secret -> envFrom -> runtime entrypoint -> native channel config
```

This works, but the non-sensitive channel policy is implicit inside entrypoint shell code. The `KarsSandbox` does not describe which channel is expected, which runtime supports it, which groups are allowed, or whether the channel is ready. Unsupported runtimes can only reject broad OpenClaw-only CLI flags; there is no reusable channel capability contract.

Feishu exposes this gap:

- OpenClaw 2026.5.27 supports Feishu through the separately published `@openclaw/feishu@2026.5.27` plugin. The package is not currently installed in the Kars image.
- Hermes Agent 0.16.0 includes a native Feishu adapter under `gateway/platforms/feishu.py`, but its optional `feishu` dependencies are not currently installed in the Kars Hermes image and Kars does not translate Feishu credentials/configuration.
- The remaining shipping runtimes do not run an IM channel gateway and must not pretend to support Feishu.
- Multiple sandboxes using the same Feishu App credentials would create competing WebSocket consumers with undefined ownership.

## 2. Decisions

The first version uses the following locked decisions:

1. **Typed channel declaration:** non-sensitive policy lives in `KarsSandbox.spec.channels[]`.
2. **Secret separation:** Feishu App ID and App Secret live in a dedicated immutable, versioned Secret, never in the CR or ConfigMap.
3. **WebSocket only:** both OpenClaw and Hermes establish outbound Feishu long connections. No webhook, public ingress, verification token, or encrypt key is supported in v1.
4. **Safe access defaults:** direct messages use `Pairing`; groups use `Allowlist`; group messages require a direct bot mention by default.
5. **One App per sandbox:** one Feishu App credential pair binds to one `KarsSandbox`. One bot may serve multiple users and multiple allowlisted groups.
6. **Two first-class adapters:** OpenClaw and Hermes support Feishu. Other runtimes fail closed with `ChannelReady=False/UnsupportedByRuntime`.
7. **No wake-from-zero:** the channel runs inside the runtime Pod. A suspended sandbox has no WebSocket consumer and cannot wake from an incoming Feishu message.
8. **No implicit egress widening:** Feishu hosts go through the existing Learn -> approve -> Strict workflow.

## 3. Goals

1. Provide one operator-facing Feishu configuration for OpenClaw and Hermes.
2. Keep credentials isolated per sandbox and out of declarative policy objects.
3. Support direct messages and multiple group chats through one Feishu App.
4. Make unsupported runtime/channel combinations visible at admission or status.
5. Preserve the existing credential rotation workflow.
6. Report whether the declared channel was translated and whether the runtime established it successfully.
7. Keep the design reusable for later DingTalk, WeCom, and other channel adapters.
8. Preserve backward compatibility for existing environment-only Telegram, Slack, Discord, and WhatsApp configurations.

## 4. Non-goals

The first version does not:

- support Feishu webhook transport;
- expose a public channel ingress;
- wake a suspended sandbox from a message;
- allow multiple sandboxes to share one Feishu App;
- add a central Channel Gateway or message queue;
- implement Feishu for LangGraph, OpenAI Agents, Microsoft Agent Framework, Anthropic, PydanticAI, or BYO;
- add per-group overrides beyond one global allowlist and `requireMention` setting;
- support dynamic per-user agent creation;
- configure Feishu Docs, Drive, Wiki, Bitable, Calendar, or other workplace tools beyond the messaging channel;
- automatically grant or inspect Feishu tenant permissions;
- automatically approve egress domains;
- guarantee message delivery while the Pod is unavailable.

## 5. Architecture

```text
KarsSandbox.spec.channels[]
  type: Feishu
  policy only
          |
          +------------------------------+
          |                              |
          v                              v
immutable Feishu Secret          Controller capability check
FEISHU_APP_ID                    OpenClaw/Hermes -> supported
FEISHU_APP_SECRET                others -> UnsupportedByRuntime
          |                              |
          +---------------+--------------+
                          v
                 runtime container env
                          |
             +------------+-------------+
             |                          |
             v                          v
       OpenClaw adapter             Hermes adapter
 @openclaw/feishu package     native gateway/platforms/feishu.py
 openclaw.json translation      Hermes config/env translation
             |                          |
             +------------+-------------+
                          v
               outbound WebSocket via
            Kars transparent egress proxy
                          |
                          v
                 Feishu Open Platform
```

The Controller remains responsible for runtime capability and status. It does not parse Feishu messages, hold Feishu credentials, or proxy plaintext channel traffic.

## 6. Proposed API

### 6.1 KarsSandbox channel declaration

Add an optional channel list:

```yaml
apiVersion: kars.azure.com/v1alpha1
kind: KarsSandbox
metadata:
  name: teaching-agent
  namespace: kars-system
spec:
  runtime:
    kind: OpenClaw
    openclaw: {}

  channels:
    - type: Feishu
      credentialSecretRef:
        name: teaching-agent-credentials
      feishu:
        domain: Feishu
        connectionMode: WebSocket
        directMessages:
          policy: Pairing
          allowFrom: []
        groups:
          policy: Allowlist
          allowFrom:
            - oc_teaching_group
            - oc_admin_group
          requireMention: true
```

`credentialSecretRef` is optional for hand-authored backward-compatible CRs. The CLI always creates a dedicated immutable Feishu Secret and writes its name here so ordinary channel/plugin rotation cannot mutate Feishu credentials in place.

### 6.2 Rust shape

```rust
pub struct KarsSandboxSpec {
    // existing fields...
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelSpec>,
}

pub struct ChannelSpec {
    pub type_: ChannelType,
    pub credential_secret_ref: Option<LocalObjectRef>,
    pub feishu: Option<FeishuChannelSpec>,
}

pub enum ChannelType {
    Feishu,
}

pub struct FeishuChannelSpec {
    pub domain: FeishuDomain,
    pub connection_mode: FeishuConnectionMode,
    pub direct_messages: DirectMessagePolicy,
    pub groups: GroupPolicy,
}

pub enum FeishuDomain {
    Feishu,
    Lark,
}

pub enum FeishuConnectionMode {
    WebSocket,
}

pub struct DirectMessagePolicy {
    pub policy: DirectMessageAccess,
    pub allow_from: Vec<String>,
}

pub enum DirectMessageAccess {
    Pairing,
    Allowlist,
    Disabled,
}

pub struct GroupPolicy {
    pub policy: GroupAccess,
    pub allow_from: Vec<String>,
    pub require_mention: bool,
}

pub enum GroupAccess {
    Allowlist,
    Disabled,
}
```

Defaults:

| Field | Default |
|---|---|
| `domain` | `Feishu` |
| `connectionMode` | `WebSocket` |
| `directMessages.policy` | `Pairing` |
| `directMessages.allowFrom` | `[]` |
| `groups.policy` | `Allowlist` |
| `groups.allowFrom` | `[]` |
| `groups.requireMention` | `true` |

An empty group allowlist means no group is admitted. It does not mean all groups.

### 6.3 Validation

Helm CEL and Controller defense-in-depth must enforce:

1. `type: Feishu` iff the `feishu` block is present.
2. `channels[]` contains at most one `Feishu` entry.
3. `connectionMode` only accepts `WebSocket` in v1.
4. `domain` accepts `Feishu` or `Lark`.
5. `directMessages.allowFrom` contains Feishu user Open IDs (`ou_...`) only.
6. `groups.allowFrom` contains Feishu chat IDs (`oc_...`) only.
7. `directMessages.policy=Allowlist` requires at least one user ID.
8. `groups.policy=Allowlist` with an empty list is valid but admits no groups.
9. `OpenClaw` and `Hermes` accept Feishu.
10. All other runtime kinds reject or degrade before Pod readiness.
11. The referenced Secret name is same-namespace in the generated runtime namespace; cross-namespace Secret references are impossible.
12. Feishu credentials cannot appear inline in the CR.

## 7. Secret contract

The credential Secret contains only sensitive Feishu application credentials:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: teaching-agent-credentials
  namespace: kars-teaching-agent
type: Opaque
stringData:
  FEISHU_APP_ID: cli_xxx
  FEISHU_APP_SECRET: redacted
```

Required keys:

| Key | Required | Purpose |
|---|---|---|
| `FEISHU_APP_ID` | yes | Feishu/Lark self-built application ID |
| `FEISHU_APP_SECRET` | yes | Application secret |

Non-sensitive policy is compiled from the CR into Controller-owned runtime environment/config values:

| Value | Source |
|---|---|
| `FEISHU_DOMAIN` | `channels[].feishu.domain` |
| `FEISHU_CONNECTION_MODE` | locked to `websocket` |
| `FEISHU_DM_POLICY` | `directMessages.policy` |
| `FEISHU_ALLOW_FROM` | `directMessages.allowFrom` |
| `FEISHU_GROUP_POLICY` | `groups.policy` |
| `FEISHU_GROUP_ALLOW_FROM` | `groups.allowFrom` |
| `FEISHU_REQUIRE_MENTION` | `groups.requireMention` |

Credentials are injected only into the runtime container. The inference router and init containers must not receive Feishu App credentials.

## 8. CLI contract

### 8.1 Create a sandbox

```bash
kars add teaching-agent \
  --runtime openclaw \
  --channels feishu \
  --feishu-app-id "$FEISHU_APP_ID" \
  --feishu-app-secret "$FEISHU_APP_SECRET" \
  --feishu-group-allow-from "oc_teaching_group,oc_admin_group" \
  --learn-egress
```

Hermes uses the same flags:

```bash
kars add teaching-hermes \
  --runtime hermes \
  --channels feishu \
  --feishu-app-id "$FEISHU_APP_ID" \
  --feishu-app-secret "$FEISHU_APP_SECRET" \
  --feishu-group-allow-from "oc_teaching_group" \
  --learn-egress
```

New flags:

```text
--feishu-app-id <id>
--feishu-app-secret <secret>
--feishu-domain <feishu|lark>                default feishu
--feishu-dm-policy <pairing|allowlist|disabled>
--feishu-allow-from <ou_ids>
--feishu-group-policy <allowlist|disabled>
--feishu-group-allow-from <oc_ids>
--feishu-require-mention / --no-feishu-require-mention
```

CLI rules:

- `--channels feishu` requires App ID and App Secret from a flag, local credential store, or environment.
- Feishu flags are valid only with `--runtime openclaw` or `--runtime hermes`.
- Unsupported runtime combinations exit non-zero before writing any resource.
- The CLI writes credentials to the Secret and policy to the KarsSandbox CR.
- The CLI must not print App Secret in dry-run, logs, summary, or errors.

### 8.2 Rotate credentials

```bash
kars credentials update teaching-agent \
  --feishu-app-id "$NEW_APP_ID" \
  --feishu-app-secret "$NEW_APP_SECRET"
```

The credentials command rotates App ID and App Secret together into a new immutable Secret marked `staged`, then changes only the Feishu `credentialSecretRef` with a JSON Patch guarded by the CR's `resourceVersion`. This prevents rotation from overwriting a concurrent channel-policy edit. Once the referenced Secret is observed, the Controller marks it `adopted`, acquires App ownership before changing the Pod template, and deletes only obsolete adopted revisions after the new runtime connection is ready. A steady-state reconcile never garbage-collects an unadopted staged revision during the create-then-patch window. Feishu rotation rejects `--no-restart` and cannot be mixed with ordinary credential updates.

## 9. OpenClaw adapter

### 9.1 Image packaging

The Kars OpenClaw image must install the exact plugin version matching the pinned OpenClaw host:

```text
openclaw@2026.5.27
@openclaw/feishu@2026.5.27
```

The plugin declares:

```text
@larksuiteoapi/node-sdk = 1.65.0
typebox = 1.1.38
zod = 4.4.3
peer openclaw >= 2026.5.27
```

The build must fail if the Feishu plugin cannot be installed or discovered. Runtime installation from npm is forbidden.

The image build verifies:

```bash
openclaw plugins list
```

and asserts plugin ID `feishu` exists.

The image installs Feishu into an immutable external-plugin stage, copies that stage into runtime state at startup, and preserves the existing minimal bundled-plugin optimization. Build-time discovery must fail closed.

### 9.2 Runtime translation

The entrypoint converts the platform contract into `openclaw.json`:

```json
{
  "channels": {
    "feishu": {
      "appId": "${FEISHU_APP_ID}",
      "appSecret": "${FEISHU_APP_SECRET}",
      "domain": "feishu",
      "connectionMode": "websocket",
      "dmPolicy": "pairing",
      "allowFrom": [],
      "groupPolicy": "allowlist",
      "groupAllowFrom": ["oc_teaching_group"],
      "requireMention": true
    }
  },
  "plugins": {
    "allow": ["kars", "feishu"],
    "entries": {
      "kars": { "enabled": true },
      "feishu": { "enabled": true }
    }
  }
}
```

The entrypoint must fail loud if exactly one of App ID/App Secret is present or if the plugin is missing. It must not silently drop the channel.

## 10. Hermes adapter

### 10.1 Image packaging

Hermes Agent 0.16.0 includes the Feishu platform adapter but declares its libraries as optional dependencies:

```text
lark-oapi == 1.5.3
qrcode == 7.4.2
```

The Kars Hermes image must install the pinned Feishu extra or equivalent exact dependencies:

```text
hermes-agent[feishu] == 0.16.0
```

This replaces the assumption that installing `hermes-agent==0.16.0` alone makes Feishu operational.

### 10.2 Runtime translation

Hermes 0.16.0 reads these environment variables natively:

```text
FEISHU_APP_ID
FEISHU_APP_SECRET
FEISHU_DOMAIN
FEISHU_CONNECTION_MODE
```

The Kars entrypoint also writes the non-sensitive policy through `hermes config set` or the Hermes YAML configuration using the exact keys supported by the pinned package. The implementation must inspect and test the pinned adapter before choosing key names for:

```text
direct-message policy
allowed users
group policy
allowed groups
require mention
```

The spec does not guess those keys. If Hermes lacks a native setting for one platform policy, the Kars Hermes adapter must enforce it before dispatching the message to the model or mark the feature unsupported. It may not silently weaken Pairing, group Allowlist, or mention gating.

Hermes startup fails loud if credentials are partial or its Feishu optional dependencies are unavailable.

## 11. Runtime capability contract

Add a platform-owned capability table:

| Runtime | Feishu | Reason |
|---|---|---|
| OpenClaw | Supported | Pinned `@openclaw/feishu` plugin |
| Hermes | Supported | Native Feishu adapter + pinned optional dependencies |
| OpenAIAgents | Unsupported | No channel gateway daemon |
| MicrosoftAgentFramework | Unsupported | No Kars channel adapter |
| LangGraph Python/TypeScript | Unsupported | No Kars channel adapter |
| Anthropic | Unsupported | No Kars channel adapter |
| PydanticAi | Unsupported | No Kars channel adapter |
| BYO | Unsupported in v1 | No image capability declaration mechanism yet |
| SemanticKernel | Runtime adapter itself deferred | N/A |

Capability validation exists in two layers:

1. CLI rejects unsupported combinations before applying resources.
2. Controller validates the CR defensively and sets `ChannelReady=False/UnsupportedByRuntime` without creating a ready runtime Pod.

A future BYO contract version may declare channel capabilities, but v1 does not trust arbitrary images to claim Feishu support.

## 12. Group chat model

One Feishu App may serve multiple groups through one sandbox:

```text
Feishu App teaching-bot
  -> KarsSandbox teaching-agent
      -> group oc_teaching_a
      -> group oc_teaching_b
      -> group oc_admin
```

The v1 behavior is:

1. The group `chat_id` must appear in `groups.allowFrom`.
2. The message must directly mention the bot when `requireMention=true`.
3. `@all` does not count as a direct bot mention.
4. Users in an admitted group are accepted according to the runtime adapter's group policy. Per-user group allowlists are out of scope.
5. Messages from unknown groups are dropped before an LLM call and recorded as a policy denial without logging message content.
6. One bot may participate in many groups, but the same App credentials must not be active in multiple sandboxes.

## 13. One-App-one-Sandbox invariant

Kars cannot prove globally that an external secret value is unique without reading and indexing secrets across namespaces. V1 uses layered enforcement:

1. CLI local configuration warns or rejects when the same App ID is already associated with another known sandbox in the current cluster.
2. The Controller records a SHA-256 fingerprint of App ID only, never App Secret, in an internal ownership annotation or index ConfigMap.
3. Conflicting claims set `ChannelReady=False/AppAlreadyClaimed` and prevent the second runtime from starting its Feishu channel.
4. Secret values and fingerprints never appear in CR status, events, metrics, or user-visible logs.
5. Deleting a sandbox releases its ownership record through finalizer cleanup.

If cluster-wide secret indexing is considered too invasive during implementation review, the minimum acceptable v1 behavior is an explicit warning plus documentation. Silent multi-consumer use is not acceptable.

## 14. Status

Add a `ChannelReady` condition for every declared channel.

| Status | Reason | Meaning |
|---|---|---|
| `True` | `Configured` | Policy translated, credentials complete, runtime adapter reported connected |
| `False` | `CredentialsMissing` | App ID or App Secret is absent |
| `False` | `CredentialsPartial` | Exactly one required credential is present |
| `False` | `UnsupportedByRuntime` | Runtime has no Feishu adapter |
| `False` | `Connecting` | Configuration is valid; WebSocket has not connected yet |
| `False` | `ConnectionFailed` | Runtime failed to start or maintain the adapter; runtime logs distinguish plugin, dependency, authentication, and egress causes without exposing credentials |
| `False` | `AppAlreadyClaimed` | Another sandbox owns the App ID fingerprint |
| `False` | `PolicyInvalid` | IDs or access policy failed validation |
| `False` | `Suspended` | Runtime is intentionally scaled to zero; no channel consumer exists |

`Ready=True` requires every declared channel to be `ChannelReady=True`, except when the sandbox is explicitly suspended. A channel failure must not be hidden behind a healthy inference router.

The runtime reports channel state to a localhost Router/Controller-visible endpoint or writes a non-sensitive readiness artifact. The Controller must not infer `Configured` solely from Pod readiness or Secret existence.

## 15. Egress

Feishu WebSocket and REST calls run under UID 1000 and use the router's explicit HTTP CONNECT proxy. The pinned plugin receives a source-anchored Axios/WebSocket agent patch because the upstream REST bootstrap otherwise emits absolute-form HTTPS requests that the forward proxy cannot tunnel safely.

V1 does not auto-add Feishu domains. Operators use:

```bash
kars add teaching-agent ... --learn-egress
kars egress teaching-agent --learned
kars egress teaching-agent --pending
kars egress teaching-agent --approve <observed-host>
kars egress teaching-agent --enforce
```

Requirements:

- blocklist enforcement remains active in Learn mode;
- the channel reconnects after the proxy's tunnel lifetime/idle limits;
- Strict mode must deny unapproved Feishu/Lark hosts;
- approval is based on observed exact hosts or reviewed parent domains;
- `domain=Feishu` and `domain=Lark` are tested separately;
- logs and learned-host records never contain App credentials or message content.

## 16. Persistence and lifecycle

Channel credentials are already persistent Kubernetes Secret data. Channel session state and pairing state are runtime filesystem state:

| State | Location | Persistence requirement |
|---|---|---|
| App ID / App Secret | Kubernetes Secret | Survives Pod recreation |
| Typed access policy | KarsSandbox CR | Survives Pod recreation |
| Generated runtime config | Runtime workspace | Regenerated from CR + Secret on boot |
| Pairing approvals | Runtime-specific `/sandbox` path | Requires `spec.storage.workspace` PVC to survive Pod recreation |
| Conversation history | Runtime-specific `/sandbox` path | Requires PVC or external store |
| WebSocket connection | Process memory | Re-established after restart |
| Dedup cursor/state | Runtime-specific state | Requires PVC if the runtime stores it under `/sandbox` |

For production Feishu agents, CLI should recommend `--workspace-storage`. It must not falsely imply that configuring a channel automatically persists pairing or conversation state.

When `spec.suspended=true`:

- `ChannelReady=False/Suspended`;
- the WebSocket disconnects;
- Feishu messages do not wake the sandbox;
- delivery behavior while offline follows Feishu platform semantics and is not guaranteed by Kars.

## 17. Security

1. App Secret is never serialized into CRs, ConfigMaps, status, events, metrics, command summaries, dry-run output, or audit messages.
2. Only the runtime container receives Feishu credentials.
3. The inference router, egress-guard, workspace bootstrap, and other init containers do not receive the Secret.
4. Partial credentials fail closed.
5. DM Pairing and group Allowlist are the defaults.
6. Group mention gating occurs before an LLM call.
7. Runtime adapters may not weaken typed platform policy.
8. Secret rotation restarts only the target sandbox.
9. Runtime logs redact App ID, App Secret, authorization headers, event bodies, and message attachments.
10. Channel health exposes only state and error categories.
11. Webhook verification/encryption fields are rejected in v1 rather than ignored.
12. The channel process remains subject to seccomp, read-only rootfs, UID 1000, egress allowlist, and token budgets for model calls.

## 18. Error handling

| Failure | Behavior |
|---|---|
| App credentials missing | Do not start channel; `CredentialsMissing` |
| One credential missing | Do not start channel; `CredentialsPartial` |
| OpenClaw plugin absent | Fail runtime channel bootstrap; `PluginMissing` |
| Hermes Feishu extra absent | Fail runtime channel bootstrap; `AdapterDependencyMissing` |
| Invalid user/group ID | Admission or Controller `PolicyInvalid` |
| Unsupported runtime | No ready Pod; `UnsupportedByRuntime` |
| WebSocket authentication failure | Retry with bounded backoff; `ConnectionFailed` |
| WebSocket transient disconnect | Reconnect with jitter; `Connecting` during recovery |
| Egress denied | Remain disconnected; expose host-only egress denial and `ConnectionFailed` |
| Duplicate event | Drop through runtime dedup state; no second LLM call |
| Secret rotated | Restart target Pod; reconnect with new credentials |
| App ID already owned | Prevent second channel consumer; `AppAlreadyClaimed` |

Retries must be bounded and jittered. Logs must not include message content or credentials.

## 19. Testing

### 19.1 CRD and capability tests

- Feishu default values serialize in camelCase/PascalCase correctly.
- Exactly one Feishu entry is allowed.
- Webhook or unknown connection modes are rejected.
- Invalid `ou_` and `oc_` identifiers are rejected.
- OpenClaw and Hermes combinations are accepted.
- Other runtime combinations produce `UnsupportedByRuntime`.
- Inline credential fields are absent from schema.
- Helm and Rust-generated schema validation remains in parity.

### 19.2 CLI tests

- `kars add` creates CR policy and Secret credentials separately.
- Dry-run never emits App Secret.
- Missing/partial credentials fail before resource writes.
- Unsupported runtime combinations exit non-zero.
- `credentials update` merges and rotates both credential keys.
- Summaries show channel type/policy without secret values.
- Existing Telegram/Slack/Discord/WhatsApp flows remain unchanged.

### 19.3 OpenClaw image and entrypoint tests

- Image contains the exact pinned `@openclaw/feishu` plugin.
- Plugin discovery succeeds from the immutable external plugin stage while the bundled tree remains pruned.
- Complete credentials generate `channels.feishu` and plugin allow/entry blocks.
- Partial credentials fail loud.
- Pairing, group allowlist, and mention policy translate exactly.
- No secret appears in generated logs.
- WebSocket reconnect works through the Kars transparent proxy.

### 19.4 Hermes image and entrypoint tests

- Image contains `lark-oapi==1.5.3` and `qrcode==7.4.2` or equivalent pinned Feishu extra.
- Native adapter imports successfully.
- Credentials and domain/connection mode reach the adapter.
- Typed DM/group policy is enforced without weakening.
- Partial credentials or missing dependencies fail loud.
- Existing channel adapters remain functional.

### 19.5 End-to-end tests

Run separate OpenClaw and Hermes sandboxes against dedicated test Feishu Apps:

1. WebSocket connects and `ChannelReady=True`.
2. Allowed DM enters Pairing flow and approved user can chat.
3. Unknown DM cannot trigger an LLM call before pairing.
4. Allowed group with direct bot mention receives a response.
5. Allowed group without mention receives no response.
6. Unknown group receives no response.
7. `@all` alone does not trigger a response.
8. Text, image, and file message behavior is verified at the declared support level.
9. Pod restart reconnects automatically.
10. Credential rotation reconnects only the target sandbox.
11. Strict egress denies unapproved hosts; approved hosts restore connection.
12. Two sandboxes attempting one App ID produce `AppAlreadyClaimed` or an explicit supported warning path.
13. Suspended sandbox reports `ChannelReady=False/Suspended` and does not claim message wake-up support.
14. PVC-backed sandbox preserves pairing/dedup state across Pod recreation when the runtime stores that state under `/sandbox`.

Tests use dedicated non-production Feishu tenants/apps and never record credential values or message bodies in fixtures.

## 20. Documentation updates

Implementation must update:

- `docs/channels-plugins.md` with Feishu setup, permissions, group IDs, pairing, egress, and rotation;
- `docs/cli-reference.md` with the new flags;
- `docs/runtimes.md` and `docs/hermes-plugin.md` with runtime capability differences;
- `docs/security.md` with channel credential and message trust boundaries;
- `docs/api/crd-reference.md` and `docs/api/conditions.md` with typed channels and `ChannelReady`;
- image versioning documentation with the OpenClaw plugin and Hermes extra pins;
- troubleshooting guidance for WebSocket auth, group mention, allowlist, reconnect, and duplicate App ownership.

## 21. Rollout

1. Land schema and capability validation behind a disabled-by-default feature gate if CRD evolution requires staged rollout.
2. Build and scan OpenClaw/Hermes images with the pinned Feishu dependencies.
3. Run unit and image-shape tests.
4. Validate one dedicated OpenClaw test App in Learn mode.
5. Validate one dedicated Hermes test App in Learn mode.
6. Approve reviewed egress hosts and repeat in Strict mode.
7. Enable Feishu CLI flags for production use.
8. Monitor connection failures, reconnect counts, egress denials, and duplicate event drops without message-content labels.

Rollback removes the channel declaration and credentials from the selected sandbox, restarts only that Deployment, and leaves the rest of the runtime unaffected.

## 22. Acceptance criteria

The feature is complete when:

1. The same typed Feishu policy works for OpenClaw and Hermes.
2. App credentials exist only in the per-sandbox Secret.
3. Both runtimes use outbound WebSocket transport and require no public ingress.
4. OpenClaw image contains the exact compatible Feishu plugin.
5. Hermes image contains the exact compatible Feishu optional dependencies.
6. DM defaults to Pairing.
7. Groups default to Allowlist with direct mention required.
8. One App can serve multiple allowlisted groups in one sandbox.
9. Unsupported runtime combinations fail visibly before readiness.
10. Missing or partial credentials fail closed.
11. `ChannelReady` reflects connection state rather than only configuration presence.
12. Egress remains operator-approved and no channel host is silently allowlisted.
13. Secret rotation restarts only the target sandbox and does not leak credentials.
14. Existing Discord, Telegram, Slack, and WhatsApp behavior does not regress.
15. No wake-from-zero capability is claimed.
16. OpenClaw and Hermes Feishu E2E scenarios pass with dedicated test Apps.

## 23. Future extensions

- central Channel Gateway for one App routing to multiple sandboxes;
- message queue and wake-from-zero;
- webhook transport with public ingress, signature verification, encryption, and replay protection;
- DingTalk and WeCom adapters using the same typed contract;
- BYO runtime channel capability declarations in a future contract version;
- per-group user policies and per-group mention overrides;
- dynamic per-user agent creation;
- channel-level rate limits, quotas, and delivery SLOs;
- centralized deduplication and dead-letter handling;
- richer Feishu workplace tools under separately governed tool capabilities.
