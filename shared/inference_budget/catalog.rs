// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    tariffs::{ModelContract, Operation, Quote},
    types::*,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Catalog {
    pub version: String,
    pub contracts: Vec<ModelContract>,
    /// Operator-declared non-inference destinations for mediated HTTP egress. This is
    /// not a free-cost assertion: those costs are outside governed inference.
    #[serde(default)]
    pub non_inference_egress_hosts: Vec<String>,
}

impl Catalog {
    pub fn validate(&self, _now: i64) -> Result<(), BudgetError> {
        if !valid_uid(&self.version)
            || self.contracts.is_empty()
            || self.contracts.len() > 64
            || self.non_inference_egress_hosts.len() > 64
        {
            return Err(BudgetError::Contract);
        }
        let mut routes = BTreeSet::new();
        let mut versions = BTreeSet::new();
        for contract in &self.contracts {
            contract.validate(i64::MIN)?;
            let route = format!(
                "{}|{}|{}|{:?}",
                contract.provider_id,
                contract.endpoint.trim_end_matches('/'),
                contract.model,
                contract.operation
            );
            if !routes.insert(route)
                || !versions.insert((contract.id.clone(), contract.version.clone()))
            {
                return Err(BudgetError::Contract);
            }
        }
        for host in &self.non_inference_egress_hosts {
            if !valid_name(host)
                || host != &host.to_ascii_lowercase()
                || self
                    .contracts
                    .iter()
                    .any(|contract| endpoint_host(&contract.endpoint) == Some(host.as_str()))
            {
                return Err(BudgetError::Contract);
            }
        }
        Ok(())
    }

    pub fn select(
        &self,
        provider_id: &str,
        endpoint: &str,
        model: &str,
        operation: Operation,
        now: i64,
    ) -> Result<&ModelContract, BudgetError> {
        self.validate(now)?;
        let selected = self
            .contracts
            .iter()
            .find(|contract| {
                contract.provider_id == provider_id
                    && contract.endpoint.trim_end_matches('/') == endpoint.trim_end_matches('/')
                    && contract.model == model
                    && contract.operation == operation
            })
            .ok_or(BudgetError::Contract)?;
        selected.validate(now)?;
        Ok(selected)
    }

    /// The broker validates quotes against its own current operator catalog.
    /// A caller cannot supply its own maximum rates, expiry, or token bounds.
    pub fn accepts_quote(
        &self,
        quote: &Quote,
        now: i64,
        money_required: bool,
    ) -> Result<(), BudgetError> {
        let stored = self.select(
            &quote.contract.provider_id,
            &quote.contract.endpoint,
            &quote.contract.model,
            quote.contract.operation,
            now,
        )?;
        if stored != &quote.contract {
            return Err(BudgetError::Contract);
        }
        quote.validate(now, money_required)
    }

    pub fn allows_mediated_non_inference(
        &self,
        host: &str,
        configured_model_hosts: &[String],
    ) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.non_inference_egress_hosts
            .iter()
            .any(|allowed| allowed == &host)
            && !configured_model_hosts
                .iter()
                .any(|model| model.eq_ignore_ascii_case(&host))
            && !self
                .contracts
                .iter()
                .any(|contract| endpoint_host(&contract.endpoint) == Some(host.as_str()))
    }
}

pub fn endpoint_host(endpoint: &str) -> Option<&str> {
    let (_, rest) = endpoint.split_once("://")?;
    let authority = rest.split('/').next()?;
    if authority.is_empty() || authority.contains('@') || authority.starts_with('[') {
        return None;
    }
    Some(authority.split(':').next()?)
}

/// Exact, closed final-dispatch path classification. No substring such as
/// "completion" grants access to an unimplemented provider operation.
pub fn operation(path: &str) -> Option<Operation> {
    if path.contains(['?', '#']) {
        return None;
    }
    let path = path.trim_matches('/');
    match path {
        "chat/completions" | "v1/chat/completions" | "openai/v1/chat/completions" => {
            Some(Operation::ChatCompletions)
        }
        "messages" | "v1/messages" | "anthropic/v1/messages" => Some(Operation::AnthropicMessages),
        "responses" | "v1/responses" | "openai/v1/responses" => Some(Operation::Responses),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_budget_contract::tariffs::{MaximumPrice, OutputField};

    fn catalog() -> Catalog {
        Catalog {
            version: "catalog-v1".into(),
            contracts: vec![ModelContract {
                id: "chat".into(),
                version: "v1".into(),
                valid_until: "2030-01-01T00:00:00Z".into(),
                provider_id: "provider".into(),
                endpoint: "https://models.example".into(),
                model: "model".into(),
                operation: Operation::ChatCompletions,
                output_field: OutputField::MaxTokens,
                maximum_input_tokens: 100,
                maximum_output_tokens: 50,
                maximum_wire_bytes: 4096,
                output_bound_includes_reasoning: true,
                maximum_price: Some(MaximumPrice::PerRequest { maximum_micros: 10 }),
            }],
            non_inference_egress_hosts: vec!["api.github.com".into()],
        }
    }

    #[test]
    fn no_provider_model_or_operation_fallback_implicitly_acquires_a_contract() {
        let catalog = catalog();
        assert!(
            catalog
                .select(
                    "provider",
                    "https://models.example",
                    "model",
                    Operation::ChatCompletions,
                    1
                )
                .is_ok()
        );
        assert!(
            catalog
                .select(
                    "other",
                    "https://models.example",
                    "model",
                    Operation::ChatCompletions,
                    1
                )
                .is_err()
        );
        assert!(
            catalog
                .select(
                    "provider",
                    "https://other.example",
                    "model",
                    Operation::ChatCompletions,
                    1
                )
                .is_err()
        );
        assert!(
            catalog
                .select(
                    "provider",
                    "https://models.example",
                    "other-model",
                    Operation::ChatCompletions,
                    1
                )
                .is_err()
        );
        assert!(
            catalog
                .select(
                    "provider",
                    "https://models.example",
                    "model",
                    Operation::Responses,
                    1
                )
                .is_err()
        );
    }

    #[test]
    fn opaque_model_tunnels_are_not_non_inference_cost_exclusions() {
        let catalog = catalog();
        assert!(catalog.allows_mediated_non_inference("api.github.com", &[]));
        assert!(!catalog.allows_mediated_non_inference("models.example", &[]));
        assert!(!catalog.allows_mediated_non_inference("unknown.example", &[]));
        assert!(
            !catalog.allows_mediated_non_inference("api.github.com", &["api.github.com".into()])
        );
    }

    #[test]
    fn unsupported_hidden_or_async_generation_never_matches_a_text_contract() {
        for path in [
            "embeddings",
            "completions",
            "images/generations",
            "agents/run",
            "openai/fine-tuning/jobs",
            "memory_stores/search",
            "openai/containers",
            "knowledgebases/retrieve",
        ] {
            assert!(operation(path).is_none(), "{path}");
        }
        assert_eq!(operation("/v1/responses"), Some(Operation::Responses));
        assert_eq!(
            operation("/anthropic/v1/messages"),
            Some(Operation::AnthropicMessages)
        );
    }
}
