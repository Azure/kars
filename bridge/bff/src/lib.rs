// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — library surface.
//
// The crate is split into a thin binary (`main.rs`) and this library so the
// HTTP router and supporting modules can be exercised directly in integration
// tests without binding a socket.

pub mod auth;
pub mod config;
pub mod error;
#[path = "../../../shared/eval_names.rs"]
pub(crate) mod eval_names;
#[path = "../../../shared/inference_budget/mod.rs"]
pub mod inference_budget_contract;
#[cfg(test)]
#[path = "../../../shared/inference_budget/dispatch.rs"]
mod inference_budget_dispatch;
pub mod kars;
mod providers;
pub mod routes;
#[path = "../../../shared/skill_package.rs"]
pub(crate) mod skill_package;
pub mod state;
#[path = "../../../shared/team_memory.rs"]
pub(crate) mod team_memory;
