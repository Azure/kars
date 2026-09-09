// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::Client;
use std::sync::Arc;

pub struct Fence {
    pub client: Arc<Client>,
    pub model_hosts: Vec<String>,
}

impl Fence {
    pub async fn check(&self, host: &str) -> Result<(), String> {
        let catalog = self.client.catalog().await.map_err(|_| {
            "Governed inference budget unavailable; mediated egress is closed".to_owned()
        })?;
        if catalog.allows_mediated_non_inference(host, &self.model_hosts) {
            Ok(())
        } else {
            Err("Governed inference requires a brokered model route; this destination is not an operator-declared non-inference exclusion".into())
        }
    }
}

pub fn model_hosts(config: &crate::config::Config) -> Vec<String> {
    [
        config.azure_openai_endpoint.as_deref(),
        config.foundry_endpoint.as_deref(),
        config.foundry_project_endpoint.as_deref(),
        Some(config.anthropic_endpoint.as_str()),
        config.ollama_endpoint.as_deref(),
    ]
    .into_iter()
    .flatten()
    .chain(
        config
            .providers
            .values()
            .map(|provider| provider.endpoint.as_str()),
    )
    .filter_map(crate::proxy::endpoint_host)
    .collect()
}
