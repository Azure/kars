// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! AzureClaw Inference Router — library crate.
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
pub mod audit;
pub mod audit_jsonl;
pub mod audit_sink;
pub mod auth;
pub mod behavior_monitor;
pub mod blocklist;
pub mod budget;
pub mod config;
pub mod copilot_auth;
pub mod deployment_health;
pub mod egress_allowlist_loader;
pub mod egress_blocked;
pub mod errors;
pub mod failover;
pub mod forward_proxy;
pub mod governance;
pub mod handoff;
pub mod inference_policy_loader;
pub mod mcp;
pub mod memory_binding_loader;
pub mod mesh;
pub mod metrics;
pub mod policy_envelope;
pub mod policy_status;
pub mod providers;
pub mod proxy;
pub mod rate_limiter;
pub mod routes;
pub mod safety;
pub mod spawn;
pub mod telemetry;
