// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Signed Merkle audit anchors — **future work, not yet wired in**.
//!
//! This module ships a complete pure-function library for signed
//! Merkle anchors over batches of audit entries (see [`merkle`]).
//! It is **not** currently called from the live router audit
//! pipeline. The runtime audit path produces a linear SHA-256 hash
//! chain via [`crate::audit_jsonl`] and [`crate::audit_sink`]; that
//! chain provides tamper-*detection*, not non-repudiation.
//!
//! Wiring this module in (anchor-on-rotation, signed root export)
//! is tracked on the roadmap. Until then, treat anything below as
//! a reusable library, not as a description of what the router
//! does on the hot path.

pub mod merkle;
