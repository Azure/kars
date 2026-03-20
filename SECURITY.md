# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in AzureClaw, please report it responsibly.

**Do NOT open a GitHub issue for security vulnerabilities.**

Instead, please report security vulnerabilities through the Microsoft Security Response Center (MSRC):

- **Web:** https://msrc.microsoft.com/create-report
- **Email:** secure@microsoft.com

Please include:
- Description of the vulnerability
- Steps to reproduce
- Impact assessment
- Any suggested mitigations

We will acknowledge receipt within 24 hours and provide a detailed response within 72 hours.

## Security Design

AzureClaw implements defense-in-depth with multiple layers:

1. **Azure Infrastructure** — DDoS protection, NSG, AKS API server authorized IPs
2. **Azure Linux** — SELinux-enforcing node OS with automatic patching
3. **Kata VM Isolation** — Per-pod dedicated kernel (`--isolation confidential`)
4. **Container Hardening** — Read-only rootfs, non-root, no privilege escalation, drop ALL capabilities
5. **Kernel Confinement** — Custom seccomp syscall filter (azureclaw-strict)
6. **Network Segmentation** — Default-deny NetworkPolicy + iptables UID-based per-container egress control
7. **Inference Safety** — Azure AI Content Safety + Prompt Shields + token budgets

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.x (alpha) | Yes (best-effort) |

## Security Updates

Security patches are released as soon as possible after verification. Subscribe to GitHub releases for notifications.
