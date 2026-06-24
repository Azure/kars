# Security Audit — egress-guard init container must use an image that ships iptables

PR: Azure/kars (branch `fix/egress-guard-distroless-image`)

## Scope

`controller/src/reconciler/mod.rs` — the per-sandbox pod spec's `egress-guard`
init container image. Test fixtures updated in `controller/src/reconciler/tests.rs`.

## Problem

On a current-release AKS deploy **every sandbox pod is stuck** in
`Init:CrashLoopBackOff`. Confirmed on a live cluster:

```
egress-guard  reason=StartError  exit=128
failed to create containerd task: ... exec: "sh": executable file not found in $PATH
```

The `egress-guard` init container runs `["sh", "-c", "iptables -A OUTPUT ..."]`
to install the per-pod egress lockdown (UID 1000 → loopback + DNS only, :80/:443
NAT-redirected to the inference-router on :8444). It was configured to use the
**inference-router image**, which became **AL3 distroless** (no shell, no
`iptables`) in #383 — the same distroless migration that broke the controller
probes (fixed in v0.1.10). So the init container cannot even start, and because
the egress guard is an init container, the agent + router containers never run.

## Fix

Point the `egress-guard` init container at the **sandbox image**
(`ctx.sandbox_image`) instead of the distroless router image. The sandbox base
image installs `iptables` + `util-linux` and ships a shell
(`sandbox-images/openclaw/Dockerfile.base:171`), and it is already pulled on the
node (it is the agent container image), so there is no new image, no extra pull,
and no new registry/credential. This is also the *same* image whose entrypoint
performs the equivalent iptables setup in Docker mode, so K8s now matches Docker.

## Threat model

### T1: New asset source / attacker-reachable input? (NO)
No new image is introduced. `ctx.sandbox_image` is the already-trusted,
already-pulled agent image (cosign-verified via the release pipeline). The
iptables command string is unchanged.

### T2: Security-control change? (RESTORES the control, no weakening)
The egress guard is a security control (egress lockdown). It was previously
**completely non-functional** (the init crashlooped, so no sandbox ran at all —
fail-closed: no agent, but also no working product). This change makes the guard
actually execute. The init container's privilege envelope is unchanged
(`runAsUser:0`, `NET_ADMIN`+`NET_RAW`, `seccomp:Unconfined`, all other caps
dropped) — the minimum required to write iptables rules in the pod netns — and
the iptables ruleset itself is byte-for-byte identical. The main agent container
still runs as UID 1000 with the egress lock applied.

### T3: Does using the larger sandbox image widen attack surface? (NO meaningful change)
The sandbox image already runs in the same pod as the agent container, so its
contents are already present in the pod. The init container exits after applying
iptables; it does not stay running. No new capability or mount is added.

## Verification

- Root cause confirmed on the live cluster (StartError / `exec: "sh" not found`,
  egress-guard using `kars-inference-router:latest`; sandbox base confirmed to
  install `iptables`).
- `cargo build --release` + full controller test suite pass.
- Will be re-verified live: after the v0.1.12 controller image rolls out, the
  egress-guard init runs and the sandbox reaches Running.

## Verdict

Accept. Restores a security control that the distroless migration had silently
disabled, with no change to the privilege envelope or the iptables policy, and
no new image dependency.

Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>
