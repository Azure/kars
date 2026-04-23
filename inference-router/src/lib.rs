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

pub mod auth;
pub mod blocklist;
pub mod budget;
pub mod config;
pub mod errors;
pub mod forward_proxy;
pub mod governance;
pub mod handoff;
pub mod mesh;
pub mod metrics;
pub mod proxy;
pub mod routes;
pub mod safety;
pub mod spawn;
