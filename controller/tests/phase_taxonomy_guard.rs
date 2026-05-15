// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Regression-guard test for `crd-well-oiled-machine` Slice 0
//! "honesty events".
//!
//! ## What this enforces
//!
//! All `*_reconciler.rs` files must stamp `status.phase` from the
//! closed taxonomy in `controller/src/status/phase.rs`
//! (`PHASE_PENDING` / `PHASE_COMPILED` / `PHASE_READY` /
//! `PHASE_DEGRADED` / `PHASE_FAILED`) and **must not** hand-roll
//! string literals like `"Pending"` / `"Ready"` / `"Degraded"` /
//! `"Failed"` / `"Compiled"`.
//!
//! Without this guard a future drive-by `phase: Some("Ready".into())`
//! would silently break the principles.md §3 invariant that
//! `Ready` ⇔ router echoed digest.
//!
//! ## Why a test instead of clippy
//!
//! Clippy can't distinguish "phase string literal" from "any string
//! literal". This test reads each reconciler file and greps for
//! suspect literals in non-comment, non-test contexts.
//!
//! ## How to update
//!
//! If you genuinely need a new phase, add the constant to
//! `controller/src/status/phase.rs` and use it. Do not add to
//! `LITERAL_DENYLIST` and a new exception — the whole point is that
//! exceptions don't get added.

use std::fs;
use std::path::PathBuf;

/// String literals that must never appear in reconciler source.
const LITERAL_DENYLIST: &[&str] = &[
    "\"Pending\"",
    "\"Compiled\"",
    "\"Ready\"",
    "\"Degraded\"",
    "\"Failed\"",
];

/// Reconcilers under audit. Adding a new `*_reconciler.rs` file
/// without listing it here is fine — the directory walker picks it
/// up — but the explicit list documents the contract.
fn reconciler_files() -> Vec<PathBuf> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    fs::read_dir(&src)
        .expect("controller/src directory must exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.ends_with("_reconciler.rs"))
                .unwrap_or(false)
        })
        .collect()
}

/// Crude classifier: skip lines that are clearly comments.
/// We bias to false-negatives over false-positives because the cost
/// of a missed grep is a controller bug, while the cost of a
/// false-positive is developer friction.
fn is_excluded_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with("*")
        || t.starts_with("#[doc")
        || t.starts_with("#![doc")
}

#[test]
fn reconcilers_must_use_phase_constants_not_string_literals() {
    let files = reconciler_files();
    assert!(
        !files.is_empty(),
        "no *_reconciler.rs files discovered — broken test harness, not real pass"
    );

    let mut violations: Vec<String> = Vec::new();

    for path in &files {
        let body =
            fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));

        let mut in_test_module = false;
        let mut brace_depth_at_test_entry = 0i32;
        let mut current_depth = 0i32;

        for (idx, line) in body.lines().enumerate() {
            current_depth += line.matches('{').count() as i32;
            current_depth -= line.matches('}').count() as i32;

            if !in_test_module && line.contains("#[cfg(test)]") {
                in_test_module = true;
                brace_depth_at_test_entry = current_depth;
                continue;
            }
            if in_test_module && current_depth <= brace_depth_at_test_entry {
                in_test_module = false;
            }
            if in_test_module {
                continue;
            }
            if is_excluded_line(line) {
                continue;
            }

            for needle in LITERAL_DENYLIST {
                if line.contains(needle) {
                    violations.push(format!(
                        "{}:{}: forbidden phase literal `{}` — use the constant from \
                         controller/src/status/phase.rs",
                        path.file_name().unwrap().to_string_lossy(),
                        idx + 1,
                        needle
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "phase taxonomy violations (crd-well-oiled-machine principles.md §3 / Slice 0):\n  {}",
        violations.join("\n  ")
    );
}
