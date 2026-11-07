// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! kars Inference Router — library crate.
//!
//! Re-exports modules for integration tests. The binary entry point is `main.rs`.
#![allow(
    clippy::collapsible_if,
    clippy::redundant_guards,
    clippy::needless_borrows_for_generic_args,
    clippy::match_like_matches_macro,
    clippy::await_holding_lock,
    clippy::unnecessary_unwrap
)]

pub mod a2a;
pub mod a2a_mtls;
pub mod access_request;
pub mod audit;
pub mod audit_jsonl;
pub mod audit_sink;
pub mod auth;
pub mod behavior_monitor;
pub mod blocklist;
pub mod budget;
#[path = "../../shared/builtin_memory.rs"]
#[allow(dead_code)]
mod builtin_memory;
mod builtin_memory_client;
pub mod config;
pub mod config_mount;
#[path = "../../shared/constant_time.rs"]
mod constant_time;
pub mod copilot_auth;
pub mod deployment_health;
pub mod egress_allowlist_loader;
pub mod egress_blocked;
pub mod errors;
pub mod failover;
pub mod forward_proxy;
mod github_app;
mod github_services;
pub mod governance;
pub mod governed_services;
pub mod guardrails;
pub mod handoff;
pub mod inference_budget;
#[path = "../../shared/inference_budget/mod.rs"]
pub mod inference_budget_contract;
#[path = "../../shared/inference_budget/dispatch.rs"]
mod inference_budget_dispatch;
pub mod inference_policy_loader;
#[path = "../../shared/kube_client_config.rs"]
mod kube_client_config;
pub mod mcp;
#[path = "../../shared/memory_access.rs"]
pub mod memory_access;
pub mod memory_binding_loader;
pub mod mesh;
pub mod metrics;
#[path = "../../shared/observation_privacy.rs"]
pub mod observation_privacy;
mod observation_privacy_client;
pub mod policy_envelope;
pub mod policy_status;
#[path = "../../shared/private_tls.rs"]
mod private_tls;
pub mod provider;
pub mod providers;
pub mod proxy;
pub mod rate_limiter;
pub mod routes;
pub mod safety;
#[path = "../../shared/service_control.rs"]
pub mod service_control;
pub mod service_control_tls;
pub mod service_observation;
pub mod service_observation_tls;
#[path = "../../shared/service_observer.rs"]
pub mod service_observer;
pub mod sidecar_client;
pub mod spawn;
#[path = "../../shared/sre_privacy.rs"]
mod sre_privacy;
pub mod sre_proxy;
#[path = "../../shared/sre_report.rs"]
#[allow(dead_code)]
mod sre_report;
pub mod task_telemetry;
pub mod telemetry;

/// Select the workspace's AWS-LC backend for JWT signing and verification.
pub fn install_jsonwebtoken_crypto_provider() {
    if jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER
        .install_default()
        .is_err()
    {
        tracing::debug!("jsonwebtoken CryptoProvider already installed");
    }
}
