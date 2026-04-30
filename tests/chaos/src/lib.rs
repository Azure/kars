//! AzureClaw chaos tier — fault-injection harness (Phase 2 S16).
//!
//! This crate is empty by design: all logic lives in feature-gated
//! integration tests under `tests/`. The library exists only so the
//! workspace can compile this member when `--features chaos` is **not**
//! enabled (default `cargo test --all`).
//!
//! Run the chaos tier with:
//!
//! ```bash
//! cargo test --workspace --tests --features chaos
//! ```
//!
//! See `tests/chaos/README.md` for tier composition and operational notes.

#![cfg(feature = "chaos")]

pub mod harness;
