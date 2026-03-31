# Changelog

All notable changes to AzureClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **AGT Governance Sidecar** — Python sidecar wrapping AGT SDK v3.0.0 with PolicyEvaluator, FileTrustStore (0–1000, ±200 clamp), SHA-256 Merkle audit chain, RateLimiter, and AgentBehaviorMonitor
- **E2E Encrypted Inter-Agent Messaging** — Signal Protocol (X3DH + Double Ratchet) via AgentMesh relay/registry with KNOCK trust handshake
- **Content Safety Circuit Breaker** — fail-open with 60s auto-reset cooldown (prevents cascading failures when Content Safety endpoint is misconfigured)
- **Foundry Agent Service Integration** — web search, code execute, file search, image generation, memory via Foundry project endpoint
- **6-Image Architecture** — controller, inference-router, sandbox, agt-governance-sidecar, agentmesh-relay, agentmesh-registry
- **CLI `push --only <image> --apply`** — selective image builds with automatic pod restart
- **10 AGT Policy Rules** — shell-safety, inference rate-limiting, content safety, mesh trust gates, spawn governance
- **Operator Dashboard** — real-time trust scores, audit chain, policy status, mesh connectivity
- **GitHub CI/CD** — Rust + TypeScript + Python lint/test, Bicep validation, Helm lint, Trivy security scan, Dockerfile lint, tag-triggered releases
- **310 Unit Tests** — Rust (130), TypeScript (148), Python (32) covering controller, router, CLI, and sidecar
- **GitHub Templates** — issue templates (bug, feature, security), PR template, CODEOWNERS

### Fixed
- Sidecar bind address (127.0.0.1 → 0.0.0.0) for K8s probe accessibility
- K8s probe host field removal (kubelet defaults to pod IP)
- Missing transitive Python dependencies (typing_inspection, cryptography) via PyPI fallback
- 8 vendor patches for AgentMesh relay, registry, and SDK bugs

### Security
- Circuit breaker fails open instead of closed (prevents total service lockout)
- iptables UID-based egress — agent process restricted to localhost
- Zero Azure credentials in agent container — sidecar authenticates via Workload Identity
- Kata Confidential VM support — per-pod dedicated kernel
- Custom seccomp profile (~150 allowed syscalls)
- Domain blocklist (53k+ malicious domains)
