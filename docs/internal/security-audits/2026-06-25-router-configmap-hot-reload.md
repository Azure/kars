# Security Audit — Router ConfigMap hot-reload fix + `kars connect` wait-for-ready (v0.1.14)

Date: 2026-06-25
Scope: `inference-router/src/config_mount.rs` (new), `inference-router/src/{inference_policy_loader,egress_allowlist_loader,memory_binding_loader}.rs`, `inference-router/src/governance/mod.rs`, `inference-router/src/lib.rs`, `cli/src/commands/connect.ts`.
Gated path: `cli/src/commands/connect.ts` (capability surface).

## Summary

Two fixes:

1. **Router ConfigMap hot-reload (the load-bearing fix).** All four router
   change-detection watchers (`InferencePolicy`, `EgressAllowlist`,
   `KarsMemory` binding, AGT governance policies) used a `dir_max_mtime` built
   on `std::fs::DirEntry::metadata()` — an **`lstat`** that does **not** follow
   symlinks. Kubernetes projects ConfigMap/Secret mounts as per-key **symlinks**
   into an atomically-swapped `..data` directory; on update kubelet swaps
   `..data` but never recreates the per-key symlink, so its `lstat` mtime is
   frozen at pod start. Result: the 5s mtime poll **never detected a ConfigMap
   update**, and the router silently kept enforcing the **boot-time** policy
   until the pod was restarted. Live-confirmed in production: a patched
   `InferencePolicy` reached the ConfigMap in 22s but the router never
   reloaded in 120s; only a pod bump applied it.

   Fix: a single shared `config_mount::dir_max_mtime(dir, exts)` that stats via
   `std::fs::metadata(e.path())` (**follows** symlinks → sees the real file's
   mtime, which advances on every `..data` swap). All four watchers delegate to
   it, so the bug cannot reappear in one loader while fixed in another. A
   regression test reproduces the exact kubelet atomic `..data` symlink swap
   and is proven to FAIL on the old `lstat` code and PASS on the fix.

2. **`kars connect` wait-for-ready + free-port.** `connect` started
   `kubectl port-forward` without waiting for the pod, so a still-`Pending`
   sandbox produced a raw Node stack trace; and a busy `:18789` produced
   `EADDRINUSE`. Now it polls until the pod is `Running` + the agent container
   `Ready` (fail-fast with a clean message on `ImagePullBackOff`/
   `CrashLoopBackOff`), and auto-picks the next free local port.

## T1: New capability / attack surface? (NO)

- The hot-reload fix changes only **how a change is detected** (stat vs lstat)
  on the same already-mounted, controller-published ConfigMaps. No new mount,
  path, network listener, or privilege. Reading file content already followed
  symlinks; only the mtime probe was wrong.
- `connect` adds a read-only `kubectl get pod` poll and a localhost TCP
  bind-probe for a free port. No new cluster permission; the operator already
  holds `port-forward`.

## T2: Security-control change? (STRICTER / NEUTRAL)

- Hot-reload is a **fail-closed improvement for enforcement freshness**: prompt
  shields, content-safety floors, token budgets, egress allowlists, and AGT
  governance edits now take effect within the advertised window instead of
  silently lagging until a pod restart. Tightening an `InferencePolicy`
  (e.g. enabling Prompt Shields, lowering a severity floor, or shrinking the
  egress allowlist) previously did **not** apply to a running router — a real
  fail-**open** gap that this closes. The loaders’ digest-comparison and
  content parsing are unchanged; only change-detection is corrected.
- `connect` changes operator UX only; no enforcement, RBAC, admission, mesh, or
  auth path is touched. The token is still read from the namespaced
  `gateway-token` Secret.

## T3: Availability / fail-open risk? (REDUCED)

- `std::fs::metadata` follows symlinks with one extra `stat` per `*.json`/
  `*.yaml` file every 5s — negligible. A dangling symlink mid-swap is skipped
  (treated as "no change") rather than erroring, so a transient swap window
  cannot crash or wedge the watcher.
- `connect` fails fast with a clear message instead of a stack trace and no
  longer crashes on a busy port. Pure availability/UX improvement.

## Verification

- New `config_mount` unit tests (5) incl. `detects_configmap_data_symlink_swap`,
  which simulates kubelet's atomic `..data` rename. **Proven**: FAILS on the
  old `e.metadata()` (lstat) implementation, PASSES on `std::fs::metadata`.
- `cargo clippy -p kars-inference-router --lib` clean; **944** router lib tests
  pass (939 prior + 5 new).
- CLI: `tsc --noEmit` + oxlint clean; **821** vitest tests pass.

## Verdict

Accept. Fixes a silent fail-open in router policy-enforcement freshness with a
single shared, regression-tested helper, and removes two first-run `kars connect`
footguns. No security control is weakened; enforcement timeliness is improved.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
