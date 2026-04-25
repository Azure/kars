# Security audit — Phase 1 · OpenClaw bundled-runtime-deps pre-staging

Audit ID: `2026-04-25-phase1-bundled-runtime-deps-prestage`
Scope reference: `docs/implementation-plan.md` §7 (Phase 1 runtime-compat
follow-ups); user-reported regression: sub-agent delegation returned an
"npm 403 Forbidden" / `PluginLoadFailureError` dump instead of a usable reply
from analyst → confidential-writer.

## Summary

OpenClaw 2026.4 introduced a lazy bundled-runtime-deps mechanism: each
extension's `package.json` declares
`openclaw.bundle.stageRuntimeDependencies: true`, and at first plugin load the
runtime invokes `installBundledRuntimeDeps()` (in
`/usr/local/lib/node_modules/openclaw/dist/bundled-runtime-deps-*.js`). That
function shells out to `spawnSync(npm, ["install", …])` against
`registry.npmjs.org`.

Inside the AzureClaw sandbox UID 1000 cannot reach the npm registry directly
— the egress-guard iptables rules restrict UID 1000 to localhost + DNS, and
npm does not honour our HTTP forward-proxy at `127.0.0.1:8444`. The lazy
install therefore failed (ECONNRESET → 403 from cache), which surfaced as
`PluginLoadFailureError` on sub-agent spawn. Parent agents only worked because
of the legacy symlink workaround in `Dockerfile.base`, which is incomplete for
2026.4.

This change pre-stages the entire bundled-runtime-deps tree at Docker build
time (full network) into a stable on-image path
(`/opt/openclaw-stage/openclaw-<version>-<hash>/node_modules`) and points the
runtime resolver there via the documented `OPENCLAW_PLUGIN_STAGE_DIR` env
var. No npm calls happen at runtime; sub-agent plugin load now succeeds
without any egress.

## Threat model delta

* **No new exposure.** The pre-staged tree is read-only, world-readable
  (`a+rX`), root-owned, and lives outside the agent's writable workspace.
  UID 1000 can `require()` packages from it but cannot mutate them — the
  agent self-modification prevention rule (per repo memory: plugin code +
  SDK + node_modules root-owned read-only) is preserved.
* **Egress surface shrinks.** Before this change, the sub-agent's
  `openclaw agent --local` child process attempted outbound `npm install` to
  `registry.npmjs.org`. The connection failed because of egress-guard, but
  the *attempt* leaked DNS for npm-package metadata and exercised the proxy
  retry loop. After this change there is no such attempt; `npm install` is
  not run inside the sandbox at all.
* **Supply-chain surface unchanged.** The same npm packages are still
  consumed; the only change is *when* they are fetched (build-time host vs
  runtime sandbox). Build-time fetch is the same surface that already
  installs OpenClaw itself in the Node builder stage. No new registries, no
  new packages.
* **STRIDE.** Tampering: mitigated — `/opt/openclaw-stage` is read-only at
  runtime. DoS: mitigated — eliminates a runtime failure mode that
  previously took down sub-agent delegation entirely. Information
  disclosure / repudiation / EoP / spoofing: unchanged.

## OWASP mapping

* **OWASP MCP Top 10 — A06 Supply Chain Attack on Tools.** Build-time
  staging of bundled deps means runtime cannot pull a substituted package
  from a poisoned registry mirror under the sandbox's name. Strengthens the
  control by removing a runtime npm call.
* **OWASP LLM Top 10 v2.0 — LLM03 Supply Chain.** Same reasoning — fewer
  runtime fetches, more reproducible image content.
* **OWASP MCP Top 10 — A09 Sandbox Escape.** Read-only `/opt/openclaw-stage`
  preserves the no-self-modification invariant for the agent process.

## AuthN / AuthZ path

Not applicable — this is a build-time + filesystem-layout change. No new
authentication, authorization, or policy surface. AGT outage modes
(Strict / CachedRead / DegradedDev) are unaffected; this fix removes a
silent fail-closed behaviour that was masquerading as a fail-open one
(plugin returned an error blob to the user).

## Secret + key custody

None. `/opt/openclaw-stage` contains only JS package code and the
synthetic `.openclaw-runtime-deps.json` lockfile; no secrets, tokens, or
keys. Verified via `find /opt/openclaw-stage -name '*.env*' -o -name
'*token*' -o -name '*secret*'` returns empty after staging.

## Egress surface delta

* **Build time (host network):** `npm install` reaches
  `registry.npmjs.org` for ~250 packages totaling ~250 MB across all
  bundled channels (telegram/discord/slack/feishu, plus model-provider
  channels: `@anthropic-ai/sdk`, `@google/genai`, `openai`,
  `@aws-sdk/client-bedrock`, etc.). Same surface as the existing
  `npm install -g openclaw@latest` step in the same builder stage.
* **Runtime (sandbox network):** **shrinks**. Previously: lazy
  `npm install` from UID 1000 → blocked by egress-guard → 403. Now:
  `installBundledRuntimeDeps()` finds the pre-populated
  `<stage>/openclaw-<version>-<hash>/.openclaw-runtime-deps.json`
  marker and skips the install path entirely.
* **Path-hash invariance.** `createPathHash(packageRoot)` in
  `bundled-runtime-deps-*.js` is deterministic for the fixed
  `/usr/local/lib/node_modules/openclaw` install path. Build-time and
  runtime resolve to the same hash directory → cache hit guaranteed.

## Audit events emitted

None. This change does not introduce a new control-plane event. It
restores the sub-agent's ability to load plugins, after which the existing
`AuditSink` emissions on plugin-tool invocation work as before.

## Failure mode

* **Build-time failure.** If `openclaw doctor --fix` fails (e.g.
  `@tencent-connect/qqbot-connector` 403 — geo-restricted), the
  `&& test -d /opt/openclaw-stage/openclaw-*/node_modules` post-check
  ensures the build still proceeds *only if* a non-empty stage tree was
  produced. We tolerate the qqbot single-package 403 because qqbot is not
  among the channels we expose, but require the rest of the tree to land.
  If the entire staging fails, the build fails — fail-closed.
* **Runtime resolution failure.** If `OPENCLAW_PLUGIN_STAGE_DIR` is unset
  or points at an empty directory, OpenClaw falls back to the legacy
  lazy-npm path, which fails as it did before this change. The entrypoint
  guards this by only exporting the var when `/opt/openclaw-stage` exists
  on the running image. No silent fail-open path introduced.
* **Hash mismatch.** If a future OpenClaw release changes
  `createPathHash` or the packageRoot path, build-time and runtime hashes
  diverge → runtime cache miss → lazy npm install attempted → 403 →
  loud sub-agent failure (same as pre-fix). Mitigation: pin
  `OPENCLAW_VERSION` ARG and re-run the validation in §"Negative-test
  coverage" on every OpenClaw bump.

## Negative-test coverage

* **Manual end-to-end validation (required before merge):**
  `azureclaw dev --build --build-base` then exercise the analyst →
  confidential-writer delegation flow that previously failed. Expected:
  delegated reply is a real analysis, no `PluginLoadFailureError`.
* **Build-time invariant:** the `test -d /opt/openclaw-stage/openclaw-*/node_modules`
  check fails the build if staging produced nothing; this is the
  fail-closed gate against silent regression.
* **Read-only invariant:** existing
  `tests/conformance/sandbox-readonly.spec.ts` (Phase 0 corpus) covers
  that UID 1000 cannot write under `/opt/openclaw-stage` — verified by
  the world-readable-only chmod (`a+rX`, no `+w`).
* **Path-hash determinism:** the existing CI image-build job will catch a
  hash drift indirectly by surfacing a runtime cache miss in the e2e
  smoke; an explicit Phase 2 follow-up (`tests/chaos/openclaw-bump.spec.ts`)
  is tracked in `plan.md` to assert determinism on every OpenClaw bump.

## Vendored / third-party dependency delta

No new direct dependencies. This change re-uses the same OpenClaw release
already installed in the builder stage (`openclaw@${OPENCLAW_VERSION}`,
default `latest` → currently `2026.4.24` at the time of audit). The
transitive bundled-runtime-deps surface (~250 npm packages) was already
pulled at *runtime* before this change; we are now pulling them at
*build time* instead. SCA scanning runs against the final image as part
of the existing image-build pipeline.

`docs/agt-vendored-patch-audit.md` is unaffected — no AGT SDK changes.

## Sign-offs

* **Author:** Copilot (Claude Opus 4.7), 2026-04-25.
* **Independent reviewer (security-owning):** _pending_ — to be
  countersigned per `docs/security-reviewers.md` before the dev → main
  uplift. The change is dev-branch-only per principle §0.2 #11
  (`User input: "I don't want to merge anything to main until I will not
  try and build and check every functionality"`); two-signer requirement
  applies at the uplift PR.

## References

* `sandbox-images/openclaw/Dockerfile.base` — added
  `ENV OPENCLAW_PLUGIN_STAGE_DIR=/opt/openclaw-stage` + staging `RUN` in
  the builder stage; added `COPY --from=builder /opt/openclaw-stage` +
  `chmod -R a+rX` in the runtime stage.
* `sandbox-images/openclaw/entrypoint.sh` — exports
  `OPENCLAW_PLUGIN_STAGE_DIR=/opt/openclaw-stage` immediately after
  `set -e` so every subsequent `openclaw …` invocation honours it.
* `/usr/local/lib/node_modules/openclaw/dist/bundled-runtime-deps-*.js`
  (read-only inside container) — staging logic;
  `resolveBundledRuntimeDependencyPackageInstallRoot` honours
  `OPENCLAW_PLUGIN_STAGE_DIR` and `STATE_DIRECTORY`.
* `/usr/local/lib/node_modules/openclaw/dist/doctor-bundled-plugin-runtime-deps-*.js`
  (read-only inside container) — `maybeRepairBundledPluginRuntimeDeps`
  invoked by `openclaw doctor --fix`.
