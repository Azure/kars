# Security Policy

## Reporting a Vulnerability

**Do NOT open a GitHub issue for security vulnerabilities.**

Report through the Microsoft Security Response Center (MSRC):

- **Web:** https://msrc.microsoft.com/create-report
- **Email:** secure@microsoft.com

Include: description, reproduction steps, impact assessment, suggested mitigations.

We acknowledge receipt within 24 hours and respond within 72 hours.

## Security Design

Nine independent defense-in-depth layers, all active by default:

1. **Azure Infrastructure** — NSG, AKS API server IP allowlist, DDoS protection
2. **Azure Linux** — SELinux-enforcing nodes, automatic security patching
3. **Kata VM** (confidential) — per-pod dedicated kernel
4. **Container Hardening** — read-only rootfs, non-root (UID 1000), drop ALL capabilities
5. **Kernel Confinement** — custom seccomp profile (`azureclaw-strict`, 219 allowed syscalls, 28 explicitly blocked)
6. **Network Segmentation** — iptables UID-based egress + egress proxy with allowlist/learn mode + domain blocklist (51k+)
7. **Inference Safety** — Content Safety + Prompt Shields (circuit breaker, fail-open) + per-sandbox token budgets
8. **AGT Governance** — PolicyEngine (YAML rules) gates tool execution pre-call, TrustManager (0–1000 scoring, clamped ±200, Ed25519 signed), SHA-256 Merkle audit chain, RateLimiter (500 req/sec global, 50/sec per-agent), BehaviorMonitor. Denies sensitive file access, recon tools, cloud metadata, destructive commands.
9. **E2E Encrypted Mesh** — Signal Protocol (X3DH + Double Ratchet), KNOCK trust handshake, per-message forward secrecy via AgentMesh relay/registry

See [docs/security.md](docs/security.md) for the full breakdown.

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.x (alpha) | Best-effort |

## Security Updates

Patches released as soon as possible after verification. Subscribe to GitHub releases.
