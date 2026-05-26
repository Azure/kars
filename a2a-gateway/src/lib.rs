// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `kars-a2a-gateway` — public-ingress A2A edge.
//!
//! ## Position in the call graph (ADR-0001 #4)
//!
//! ```text
//!   external A2A caller ───TLS───▶ AGC / Gateway API
//!                                       │
//!                                       ▼
//!                          ┌────────────────────────┐
//!                          │   kars-a2a-gateway │
//!                          │   • TLS termination     │
//!                          │   • subject extraction  │
//!                          │   • replay cache        │
//!                          │   • per-subject limits  │
//!                          └──────────┬──────────────┘
//!                                     │ mTLS
//!                                     ▼
//!                          inference-router :8444
//!                                     │
//!                                     ▼
//!                                 sandbox pod
//! ```
//!
//! The gateway extracts the verified-caller subject from the
//! `X-A2A-Agent-Subject` request header, applies replay protection via
//! [`verify::ReplayCache`], rate-limits per subject, then forwards
//! over mTLS to the inference router.
//!
//! ## `[GAP-V1]` JWS verifier wiring
//!
//! The standalone JWS path (`kars_a2a_core::verify_inbound_card`)
//! is **complete and tested** as a library function — every router
//! that needs it can call it directly. The gateway *binary*, however,
//! does **not** yet run that verifier in its proxy hot path; it
//! consumes the `X-A2A-Agent-Subject` header written by an upstream
//! component (today: the cluster Gateway API mTLS handshake; tomorrow:
//! a verifying axum layer inside this binary).
//!
//! Wiring `verify_inbound_card` directly into the gateway as an
//! opt-in axum layer is tracked as a v1.1 follow-up; the placeholder
//! is the unused `kars-a2a-core` workspace dependency declared
//! in `Cargo.toml`. The `[GAP-V1]` marker is mirrored in
//! `docs/architecture/a2a-gateway.md`.
//!
//! ## Hardening checklist
//!
//! - rustls only (workspace-wide constraint, no OpenSSL).
//! - drop privs to UID 1002 after binding 8443.
//! - read-only root filesystem (Helm `securityContext`).
//! - distroless static base image (musl).
//! - seccomp `kars-strict.json` (existing profile in
//!   `policy-engine/profiles/seccomp/`).
//!
//! Each item is enforced in a different layer (binary, container,
//! K8s) so a single misconfiguration does not collapse the chain.

#![forbid(unsafe_code)]

pub mod health;
pub mod metrics;
pub mod mtls;
pub mod proxy;
pub mod proxy_app;
pub mod rate_limit;
pub mod tls;
pub mod verify;
