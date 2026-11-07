// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/// Canonical built-in corpus names shared by evaluation admission and execution.
/// The eval-corpus tests verify that every name resolves to a packaged corpus.
pub const BUILTIN_NAMES: &[&str] = &[
    "jailbreak-baseline",
    "prompt-injection-2026q1",
    "banned-tools",
    "egress-known-bad",
    "governed-egress-tunnels",
    "memory-isolation",
];
