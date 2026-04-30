// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `azureclaw-a2a-gateway` — public-ingress A2A edge.
//!
//! ## Position in the call graph (ADR-0001 #4)
//!
//! ```text
//!   external A2A caller ───TLS───▶ AGC / Gateway API
//!                                       │
//!                                       ▼
//!                          ┌────────────────────────┐
//!                          │   azureclaw-a2a-gateway │
//!                          │   • TLS termination     │
//!                          │   • JWS verify          │
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
//! The gateway's *only* authentication boundary is the JWS verifier
//! (`azureclaw_a2a_core::verify_inbound_card`). Everything downstream
//! of `verify::verify_or_reject` is treated as authenticated and
//! tagged with the verified subject claim, which the router uses as
//! the policy key for inbound A2A.
//!
//! ## Hardening checklist
//!
//! - rustls only (workspace-wide constraint, no OpenSSL).
//! - drop privs to UID 1002 after binding 8443.
//! - read-only root filesystem (Helm `securityContext`).
//! - distroless static base image (musl).
//! - seccomp `azureclaw-strict.json` (existing profile in
//!   `policy-engine/profiles/seccomp/`).
//!
//! Each item is enforced in a different layer (binary, container,
//! K8s) so a single misconfiguration does not collapse the chain.

#![forbid(unsafe_code)]

pub mod health;
pub mod metrics;
pub mod mtls;
pub mod proxy;
pub mod rate_limit;
pub mod tls;
pub mod verify;
