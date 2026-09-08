# Workspace credential sources (v1)

Credential sources are an **optional, explicit** alternative to the existing
`kars-<name>/<name>-credentials` Secret. Install the controller and CRD containing
this feature before enabling it. Namespace claim v1 is a prerequisite; complete
[namespace ownership preflight/adoption](namespace-ownership.md) first.

With `spec.credentialsRef` absent, agents retain the existing direct Secret
EnvFrom and defaults. No source is discovered or delivered merely because its
name matches a future Sandbox.

## CLI workflows

New Sandbox, with credentials stored through the existing masked local prompt:

```sh
kars credentials set telegram-token
kars add demo --channels telegram --credential-source
```

The CLI creates the workspace source **before** the Sandbox, obtains its real
API-server UID, and includes that UID in the Sandbox CREATE. A create conflict
does not overwrite a racing Sandbox. `--namespace <workspace>` selects the
workspace for the source, Sandbox, and companion policies; the default remains
`kars-system`.

For an existing Sandbox:

```sh
kars credentials update demo --use-source
kars credentials update demo --telegram-token "$NEW_TOKEN"
kars credentials update demo --remove telegram-token
```

`--use-source` migrates the entire direct credential collection once, without
changing the legacy Secret. Explicit updates override imported values. Existing
staged source values override legacy values. Removal is applied last. Migration
requires read access to the verified runtime namespace and its direct Secret;
new source-bound Sandboxes do not require runtime-namespace Secret writes.

Once bound, ordinary `credentials update` automatically updates the pinned source
in the selected workspace. Pass `--namespace` for Sandboxes outside `kars-system`.
The command requires read access to that Sandbox to choose the correct path.
Direct updates also inspect runtime namespace claim metadata when present, so a
wrong workspace or same-name conflict cannot be mistaken for a direct target.
Source updates and removals use UID/resourceVersion checks and never
delete/recreate the source. `--remove` accepts comma-separated environment keys
or credential flag names. The existing `credentials remove <key>` command still
removes a **local** stored credential.

Source mode always refreshes the runtime. `--no-restart` remains available for
direct credentials but is rejected for source changes. Source-mode operations
wait for the controller to acknowledge the current source UID/resourceVersion
in `CredentialsReady`; they do not claim success merely because an older
controller accepted a CR. API schema pruning is detected after submission.
Existing secret-value flags retain their usual shell/process-argument exposure;
the source integration passes Kubernetes Secret bodies through stdin and does
not print their values.

Explicit opt-out:

```sh
kars credentials update demo --disable-source
```

The controller stops the previous credential consumer, removes **only its owned
projection**, and returns to the unchanged direct Secret. This intentionally
reactivates the legacy collection: review it before opting out. The workspace
source remains available for the same Sandbox UID; it is not Helm-owned.

Explicit pre-CR storage is supported:

```sh
kars credentials update demo --use-source --telegram-token "$TOKEN"
kars add demo --credential-source
```

The first command stores an opted-in source, not a delivery instruction. The
second explicitly selects its UID. Abandoned unbound sources can have their
keys removed with `credentials update --use-source --remove`; normal Kubernetes
Secret deletion can remove the empty resource. Bound sources have a same-
workspace Sandbox owner reference and are garbage-collected with that Sandbox.
A different Sandbox UID cannot inherit a previous binding.

## API contract

The source is an Opaque Secret in the **same namespace as the KarsSandbox CR**:

* Name: `kars-credential-source-<sandbox-name>`.
* Annotations:
  * `kars.azure.com/credential-purpose: agent-source-v1`
  * `kars.azure.com/credential-target: <sandbox-name>`
  * `kars.azure.com/credential-workspace: <CR namespace>`
  * `kars.azure.com/credential-binding-intent: explicit-reference-v1`
* Data: the complete desired agent credential collection.

After creating it, set:

```yaml
spec:
  credentialsRef:
    name: kars-credential-source-demo
    uid: <the UID returned by the Secret CREATE>
```

There is no namespace field in the reference. The controller rejects arbitrary
source names, missing purpose/intent, different target/workspace, different UID,
non-Opaque type, foreign owner references, or stale bindings. Initial binding
records the exact Sandbox UID and runtime namespace UID, with a UID/RV-fenced
metadata-only patch. Existing binding metadata must agree; it is not overwritten
to claim another Sandbox's source.

An intentionally replaced source requires a new explicit reference.
`credentials update --use-source` selects a new **compatible** source incarnation;
ordinary updates refuse UID drift. Foreign purpose/ownership is never adopted,
even with `--use-source`.

## Collection semantics and supported keys

While a reference is present, **only the owned projection is mounted as the
agent credential collection**. The direct Secret is not a fallback. Therefore
removing a source key cannot resurrect an old same-named legacy key.

Version 1 uses the existing handoff channel/search credential allowlist:

`TELEGRAM_BOT_TOKEN`, `TELEGRAM_ALLOW_FROM`, `SLACK_BOT_TOKEN`,
`DISCORD_BOT_TOKEN`, `WHATSAPP_ENABLED`, `BRAVE_API_KEY`, `TAVILY_API_KEY`,
`EXA_API_KEY`, `FIRECRAWL_API_KEY`, `PERPLEXITY_API_KEY`.

Values must be UTF-8 without NUL; the collection is limited to 128 KiB.
Router/provider/control-plane and arbitrary process environment keys are not
accepted. In particular, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, Azure identity
keys, `AGT_*`, `KARS_*`, `NODE_OPTIONS`, and `PATH` cannot be supplied this way.
Existing direct-mode flags, including `--openai-api-key`, remain available.
Migration fails rather than silently dropping unsupported existing keys.

Credential-key overrides in runtime `extraEnv`/raw `env` conflict with source
mode and are rejected, rather than silently overriding the source. The common
agent container path supports every **wired, controller-managed** runtime,
including BYO. The projection is never added to the inference-router EnvFrom.
Overlay-managed pods and unwired runtime variants are not supported.

## Projection, refresh, and failure handling

The controller owns `<sandbox-name>-credential-projection` in the verified
runtime namespace. Its metadata binds purpose, workspace, Sandbox UID, namespace
UID, and source UID, with a namespace owner reference. Unrelated Secrets at that
name are not overwritten or adopted.

An empty metadata-only anchor is created first and sealed to its own Secret UID.
Recreating an object with copied old projection metadata does not authorize
adoption of the new UID. The controller rechecks the
Sandbox UID/RV, namespace incarnation, and source UID/RV before a UID/RV-fenced
value patch. Removed keys use explicit nulls. No force-apply is used for the
projection. Credential values and API bodies are excluded from errors/logs.

Source-owned Secrets are watched through kube's **metadata-only ownership
watch**. A 30-second reconciliation backstop covers deletion, removed ownership
markers, unbound/invalid sources, and projection drift. Projection UID/RV drives
the pod-template revision; metadata-only source changes do not restart pods.

Source mode uses `Recreate`, not a rolling update that could indefinitely retain
an old credential-bearing pod when a replacement fails. Changes pause the
verified controller Deployment before refresh. Invalid, missing, or replaced
references stop that runtime and clear only the owned projection, then report
`CredentialSourceUnavailable` and retry. They never fall back to direct
credentials. Concurrent writes/replacements and non-404 API failures propagate
honestly; retries restart with fresh identities.

Kubernetes is not a cross-resource transaction system. Watches/requeues make
revocation asynchronous; API outages, unreachable nodes, and pod termination
failures can delay it. This feature does not revoke credentials at their external
provider, erase values previously observed by an agent, or defend against a
cluster administrator deliberately bypassing namespace lifecycle controls.

## Bridge and publication boundary

This is a standalone core primitive, **not closure of Bridge RBAC**. A later
adapter must store the source in its configured workspace and include the exact
UID in launch bindings. Existing Bridge `put_credential` behavior is unchanged.
Workspace Secret permissions/admission must still protect signing and other
control-plane Secrets; no new broad Role or binding is introduced here.
