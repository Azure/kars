// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — library surface.
//
// The crate is split into a thin binary (`main.rs`) and this library so the
// HTTP router and supporting modules can be exercised directly in integration
// tests without binding a socket.

pub mod auth;
pub mod config;
pub mod error;
pub mod kars;
mod providers;
pub mod routes;
pub mod state;
