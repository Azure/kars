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

AzureClaw implements defense-in-depth with 7+ layers:

1. **Azure Infrastructure** — DDoS protection, NSG, Azure Firewall
2. **Azure Container Linux** — Immutable, SELinux-enforcing, CIS-hardened host OS
3. **Confidential Containers** — Hardware-encrypted TEE (AMD SEV-SNP / Intel TDX)
4. **Container Hardening** — Read-only rootfs, non-root, no privilege escalation
5. **Kernel Confinement** — seccomp syscall filtering + SELinux MAC (ACL-native, enforcing mode)
6. **Network Segmentation** — Kubernetes NetworkPolicy with default-deny egress
7. **Application Firewall** — Envoy L7 proxy with method/path/header filtering
8. **Inference Safety** — Azure AI Content Safety + Prompt Shields

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.x (alpha) | Yes (best-effort) |

## Security Updates

Security patches are released as soon as possible after verification. Subscribe to GitHub releases for notifications.
