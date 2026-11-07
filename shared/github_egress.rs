// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/// Direct egress that bypasses repository-scoped GitHub authorization.
/// Shared by controller admission and Bridge pre-flight; keep normalization identical.
pub fn opaque_github_egress(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "*"
        || ["github.com", "api.github.com"].iter().any(|target| {
            host == *target
                || host.strip_prefix("*.").is_some_and(|suffix| {
                    *target == suffix || target.ends_with(&format!(".{suffix}"))
                })
        })
}
