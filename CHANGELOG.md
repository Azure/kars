# Changelog

All notable changes to AzureClaw will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Bidirectional Agent Handoff** — live-migrate agents between local Docker and AKS cloud with `azureclaw handoff <name> --to cloud|local`. Supports both CLI-driven (operator) and LLM-driven (webchat) orchestration paths
- **Sub-Agent Handoff** — sub-agents are snapshotted (workspace + task state), destroyed on source, re-spawned on target, and injected with workspace + resume signal via E2E encrypted mesh
- **Stale AMID Cache Poisoning Fix** — three-layer defense: identity-based AMID rejection, prekey readiness gate, workspace inject retry with ack verification
- **Workspace Injection Pipeline** — tarball extraction with path traversal validation, `incoming/` file promotion to workspace root, `HANDOFF_FILES.md` manifest for agent discoverability
- **Handoff Decommission Cleanup** — reverse handoff deletes all cloud CRDs (parent + sub-agents); forward handoff destroys local sub-agent containers
- **Mesh Inbox Improvements** — protocol message filtering (hides handoff/ack messages), auto-decode of `file_transfer` base64 content
- **Native AGT Governance** — Rust-native governance module (replaces former Python sidecar) with PolicyEvaluator, FileTrustStore (0–1000, ±200 clamp), SHA-256 Merkle audit chain, RateLimiter, and AgentBehaviorMonitor
- **E2E Encrypted Inter-Agent Messaging** — Signal Protocol (X3DH + Double Ratchet) via AgentMesh relay/registry with KNOCK trust handshake
- **Content Safety Circuit Breaker** — fail-open with 60s auto-reset cooldown (prevents cascading failures when Content Safety endpoint is misconfigured)
- **Foundry Agent Service Integration** — web search, code execute, file search, image generation, memory via Foundry project endpoint
- **5-Image Architecture** — controller, inference-router, sandbox, agentmesh-relay, agentmesh-registry (governance runs natively in the router)
- **CLI `push --only <image> --apply`** — selective image builds with automatic pod restart
- **10 AGT Policy Rules** — shell-safety, inference rate-limiting, content safety, mesh trust gates, spawn governance, sensitive file deny, recon tool deny, cloud metadata deny
- **AGT Tool Execution Gate** — exec_command and http_fetch are evaluated by the native governance module before execution; fail-open with 2s timeout
- **Operator Dashboard** — real-time trust scores, audit chain, policy status, mesh connectivity
- **GitHub CI/CD** — Rust + TypeScript + Python lint/test, Bicep validation, Helm lint, Trivy security scan, Dockerfile lint, tag-triggered releases
- **Unit Tests** — Rust (controller + router) and TypeScript (CLI + plugin) covering controller, router, CLI, and governance
- **GitHub Templates** — issue templates (bug, feature, security), PR template, CODEOWNERS

### Fixed
- Router bind address fix for K8s probe accessibility
- K8s probe host field removal (kubelet defaults to pod IP)
- Missing transitive Python dependencies (typing_inspection, cryptography) via PyPI fallback
- 8 vendor patches for AgentMesh relay, registry, and SDK bugs
- Foundry Memory Store format — ensureMemoryStore creates full store with chat + embedding models; item format matches Foundry REST API spec

### Changed
- AGT inference rate limit bumped from 120 → 500 calls/60s (policy) and router token bucket from 100 → 500 global req/s (needed for multi-agent handoff traffic)

### Security
- Circuit breaker fails open instead of closed (prevents total service lockout)
- iptables UID-based egress — agent process restricted to localhost
- Zero Azure credentials in agent container — sidecar authenticates via Workload Identity
- Kata Confidential VM support — per-pod dedicated kernel
- Custom seccomp profile (~150 allowed syscalls)
- Domain blocklist (53k+ malicious domains)
