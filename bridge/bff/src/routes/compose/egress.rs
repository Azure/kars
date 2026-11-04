// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::ComposeEgress;

pub(super) fn normalize_proposed_egress(egress: Vec<ComposeEgress>) -> Vec<ComposeEgress> {
    let mut endpoints = std::collections::BTreeMap::<String, Option<u16>>::new();
    for endpoint in egress {
        let host = endpoint.host.to_ascii_lowercase();
        if host.contains("githubcopilot.com")
            || host.ends_with(".openai.azure.com")
            || host.ends_with(".services.ai.azure.com")
        {
            continue;
        }
        endpoints.insert(host, endpoint.port.or(Some(443)));
    }
    endpoints
        .into_iter()
        .map(|(host, port)| ComposeEgress { host, port })
        .collect()
}
