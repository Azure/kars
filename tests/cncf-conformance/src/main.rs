// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! `cncf-conformance` — render the CNCF K8s AI conformance report.
//!
//! Usage: `cargo run -p azureclaw-cncf-conformance --bin cncf-conformance`.
//! Writes `tests/cncf-conformance/CONFORMANCE-REPORT.md` and exits with
//! a non-zero status if any criterion fails.

use std::fs;

fn main() -> anyhow::Result<()> {
    let report = azureclaw_cncf_conformance::run_all_checks();
    let md = report.to_markdown();
    let out_path = azureclaw_cncf_conformance::repo_root()
        .join("tests/cncf-conformance/CONFORMANCE-REPORT.md");
    fs::write(&out_path, &md)?;
    println!("{md}");
    println!("\nWrote {}", out_path.display());
    if !report.all_passed() {
        std::process::exit(1);
    }
    Ok(())
}
