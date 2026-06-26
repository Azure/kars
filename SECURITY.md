<!-- BEGIN MICROSOFT SECURITY.MD V1.0.0 BLOCK -->

## Security

Microsoft takes the security of our software products and services seriously, which
includes all source code repositories in our GitHub organizations.

**Please do not report security vulnerabilities through public GitHub issues.**

For security reporting information, locations, contact information, and policies,
please review the latest guidance for Microsoft repositories at
[https://aka.ms/SECURITY.md](https://aka.ms/SECURITY.md).

<!-- END MICROSOFT SECURITY.MD V1.0.0 BLOCK -->

## kars-Specific Security Information

### Reporting a Vulnerability

**Do NOT open a GitHub issue for security vulnerabilities.**

Report through the Microsoft Security Response Center (MSRC):

- **Web:** https://msrc.microsoft.com/create-report
- **Email:** secure@microsoft.com

Include: description, reproduction steps, impact assessment, suggested mitigations.

We acknowledge receipt within 24 hours and respond within 72 hours.

## Security Design

Nine defense-in-depth layers — eight active by default, plus opt-in per-pod confidential-VM isolation:

1. **Azure Infrastructure** — NSG, AKS API server IP allowlist, DDoS protection
2. **Azure Linux** — SELinux-enforcing nodes, automatic security patching
3. **Kata VM (confidential) — opt-in** — per-pod dedicated kernel + AMD SEV-SNP, enabled with `KarsSandbox.spec.isolation: confidential` (`kars up`'s default isolation is `enhanced`: runc + the `kars-strict` seccomp profile). Not on by default.
4. **Container Hardening** — read-only rootfs, non-root (UID 1000), drop ALL capabilities
5. **Kernel Confinement** — custom seccomp profile (`kars-strict`) with a deny-by-default syscall allowlist
6. **Network Segmentation** — iptables UID-based egress + egress proxy with allowlist/learn mode + a large domain blocklist (tens of thousands of entries; see [`cli/blocklists/`](cli/blocklists/))
7. **Inference Safety** — Content Safety + Prompt Shields (Foundry-side guardrails, parsed from model responses) + per-sandbox token budgets
8. **AGT Governance** — PolicyEngine (YAML rules) gates tool execution pre-call, TrustManager (Ed25519-signed scoring), SHA-256 hash-chained audit log (AGT's `AuditLogger`), RateLimiter, BehaviorMonitor. Denies sensitive file access, recon tools, cloud metadata, destructive commands.
9. **E2E Encrypted Mesh** — Signal Protocol (X3DH + Double Ratchet), KNOCK trust handshake, per-message forward secrecy via AgentMesh relay/registry

See [docs/security.md](docs/security.md) for the full breakdown.

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.x (alpha) | Best-effort |

## Security Updates

Patches released as soon as possible after verification. Subscribe to GitHub releases.
