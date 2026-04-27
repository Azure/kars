//! Telemetry helpers for the inference router.
//!
//! Today this module hosts only the OTel GenAI Semantic Conventions
//! constants and typed attribute bag. Phase 1 scope (plan §7 #9) adds
//! emission helpers that thread through `tracing` and `opentelemetry`
//! — call-site wiring lands with the routes decomposition.

pub mod gen_ai;
