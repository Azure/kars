# OpenClaw upstream alignment

**TL;DR** — kars does **not** fork OpenClaw. It uses only first-class extension
points (`tools.deny` config, `api.registerTool()` plugin API, `~/.openclaw-data/extensions/`
discovery) to substitute governance-aware sub-agent tools. Every OpenClaw release
stays drop-in compatible with kars.

---

## The concern

> "Denying `sessions_spawn` / `sessions_send` and substituting our own tools
> means you've diverged from upstream OpenClaw."

## The answer

Policy ≠ fork. kars alters **which capabilities are permitted**, not **what
the capabilities do.** Every alteration goes through upstream-documented
extension points.

---

## What we actually do

### 1. Deny two native tools via upstream config

A single line in the generated `openclaw.json`:

```json
"tools": {
  "deny": ["sessions_spawn", "sessions_send"],
  "exec": { "security": "full" }
}
```

Source: [`sandbox-images/openclaw/entrypoint.sh:223`](../sandbox-images/openclaw/entrypoint.sh).

`tools.deny` is an upstream-defined configuration field. Using it is no
different from denying `exec` or a particular filesystem path — it's the
mechanism OpenClaw ships for operators who want to restrict the LLM's
capability surface.

### 2. Register alternative tools via the plugin API

The kars plugin registers ~20 governance-aware tools through the
upstream `api.registerTool()` contract:

- `cloud_offload` — ship a task to a fresh AKS sandbox
- `kars_spawn` / `kars_handoff` — create or migrate to a sub-agent
- `mesh_send` / `mesh_inbox` / `discover` — peer-to-peer messaging via AgentMesh
- …plus file, memory, and Foundry skill tools

Source: [`runtimes/openclaw/src/index.ts`](../runtimes/openclaw/src/index.ts) (all `api.registerTool({ ... })` call sites).

From the agent's perspective, the mental model is preserved: it still has
"tools for spawning sub-agents" and "tools for sending messages" — the
substrate just routes through AGT, the inference router, and AKS instead
of local subprocesses.

### 3. Load the plugin via upstream auto-discovery

The OpenClaw gateway discovers non-bundled plugins in
`~/.openclaw-data/extensions/<plugin>/`. Our sandbox image drops the plugin
there, exports `register(api)` / `activate(api)` per the upstream contract,
and gets loaded at startup like any third-party extension.

No monkey-patching. No runtime injection. No modified gateway binary.

---

## Why this pattern is correct, not a workaround

| Property | Fork | kars (this pattern) |
|---|---|---|
| Tracks upstream releases | Manual merge on every bump | Drop-in |
| Visible to operators | Hidden in diffs | One config line + one plugin dir |
| Composable with other extensions | No | Yes |
| Can be disabled per-deployment | Requires rebuild | Remove the deny list |
| Upstream review surface | Every release | None needed |

The `tools.deny` field exists **precisely** so operators can make this kind
of policy decision. If denying a tool constituted "misalignment," the field
wouldn't exist in OpenClaw's config schema.

This is the same pattern that:

- Kubernetes uses for admission controllers (deny native `privileged: true`,
  substitute a policy-aware equivalent)
- Browser extension manifests use for permission scoping
- Istio uses for egress control (deny raw egress, substitute sidecar-mediated
  traffic)

Nobody calls these "forks of the host system."

---

## What **would** be a fork

For completeness, these are the things we explicitly do **not** do:

- Patch `@openclaw/core` or the gateway binary in `vendor/`
- Monkey-patch the session manager at runtime
- Intercept transport at a level below the plugin API
- Ship a modified OpenClaw protocol on the wire

A quick check: kars does **not** vendor an OpenClaw fork. The only vendored AGT
artifact is the pinned upstream Agent Governance Toolkit build (`vendor/agt/`, a
tarball + pin, plus locally-built AGT Python wheels) — used while two pre-release
fixes are in review, switching to the published releases once they land. There is
**no in-tree fork** of OpenClaw or of AGT.

If upstream OpenClaw ever removes `tools.deny` or the `api.registerTool()`
contract, **that** is the point at which alignment would need to be
re-examined — but today, both are supported, documented extension points.

---

## Summary for reviewers

> kars uses OpenClaw's own `tools.deny` configuration and plugin API
> to substitute governance-aware sub-agent tools for the native ones. No
> OpenClaw source is modified, forked, or patched. The pattern is the
> intended use of OpenClaw's extension contract and stays compatible with
> every upstream release by design.

## References

- Native tool deny list: [`sandbox-images/openclaw/entrypoint.sh:223`](../sandbox-images/openclaw/entrypoint.sh)
- Replacement tool registrations: [`runtimes/openclaw/src/index.ts`](../runtimes/openclaw/src/index.ts) (all `api.registerTool(...)` call sites)
- Plugin manifest + entry exports: [`runtimes/openclaw/src/index.ts`](../runtimes/openclaw/src/index.ts) (`register` / `activate`)
- Plugin discovery path: `~/.openclaw-data/extensions/kars-mesh/`
- Architecture overview: [`architecture.md`](architecture.md)
- Security model: [`security.md`](security.md)
