// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Operator-declared maximum bounds, never guessed token counts or prices.

use super::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum Operation {
    ChatCompletions,
    AnthropicMessages,
    Responses,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum OutputField {
    #[serde(rename = "MaxTokens")]
    Tokens,
    #[serde(rename = "MaxCompletionTokens")]
    Completion,
    #[serde(rename = "MaxOutputTokens")]
    Output,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MaximumPrice {
    TokenRates {
        /// Maximum rate across fresh/cache-read/cache-creation input categories.
        input_micros_per_million: u64,
        /// Maximum rate across visible and reasoning output categories.
        output_micros_per_million: u64,
        fixed_micros: u64,
    },
    PerRequest {
        maximum_micros: u64,
    },
}

impl MaximumPrice {
    pub fn price(&self, input: u64, output: u64) -> Result<u64, BudgetError> {
        let amount = match self {
            Self::PerRequest { maximum_micros } => return Ok(*maximum_micros),
            Self::TokenRates {
                input_micros_per_million,
                output_micros_per_million,
                fixed_micros,
            } => {
                let input = u128::from(input)
                    .checked_mul(u128::from(*input_micros_per_million))
                    .ok_or(BudgetError::Overflow)?;
                let output = u128::from(output)
                    .checked_mul(u128::from(*output_micros_per_million))
                    .ok_or(BudgetError::Overflow)?;
                let input = input.checked_add(999_999).ok_or(BudgetError::Overflow)? / 1_000_000;
                let output = output.checked_add(999_999).ok_or(BudgetError::Overflow)? / 1_000_000;
                input
                    .checked_add(output)
                    .ok_or(BudgetError::Overflow)?
                    .checked_add(u128::from(*fixed_micros))
                    .ok_or(BudgetError::Overflow)?
            }
        };
        u64::try_from(amount).map_err(|_| BudgetError::Overflow)
    }

    fn valid(&self) -> bool {
        match self {
            Self::PerRequest { maximum_micros } => {
                *maximum_micros > 0 && *maximum_micros <= MAX_LEDGER_INTEGER
            }
            Self::TokenRates {
                input_micros_per_million,
                output_micros_per_million,
                fixed_micros,
            } => {
                (*input_micros_per_million > 0
                    || *output_micros_per_million > 0
                    || *fixed_micros > 0)
                    && [
                        *input_micros_per_million,
                        *output_micros_per_million,
                        *fixed_micros,
                    ]
                    .iter()
                    .all(|value| *value <= MAX_LEDGER_INTEGER)
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelContract {
    pub id: String,
    pub version: String,
    pub valid_until: String,
    /// Exact final named provider/authentication identity, never request headers.
    pub provider_id: String,
    pub endpoint: String,
    pub model: String,
    pub operation: Operation,
    pub output_field: OutputField,
    /// The provider-enforced maximum complete input/context bound, including
    /// hidden framing, cache input, and function schemas. Not bytes/4.
    pub maximum_input_tokens: u64,
    pub maximum_output_tokens: u64,
    pub maximum_wire_bytes: u64,
    /// Operator attestation that the output field bounds ALL output, including
    /// reasoning/thinking. Contracts without this guarantee cannot dispatch.
    pub output_bound_includes_reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_price: Option<MaximumPrice>,
}

impl ModelContract {
    pub fn validate(&self, now: i64) -> Result<(), BudgetError> {
        let endpoint = reqwest::Url::parse(&self.endpoint).map_err(|_| BudgetError::Contract)?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(BudgetError::Contract);
        }
        let expiry = chrono::DateTime::parse_from_rfc3339(&self.valid_until)
            .map_err(|_| BudgetError::Contract)?
            .timestamp();
        let field_matches = matches!(
            (self.operation, self.output_field),
            (
                Operation::ChatCompletions,
                OutputField::Tokens | OutputField::Completion
            ) | (Operation::AnthropicMessages, OutputField::Tokens)
                | (Operation::Responses, OutputField::Output)
        );
        if !valid_name(&self.id)
            || !valid_uid(&self.version)
            || self.provider_id.is_empty()
            || self.provider_id.len() > 253
            || self.model.is_empty()
            || self.model.len() > 253
            || !(self.endpoint.starts_with("https://") || self.endpoint.starts_with("http://"))
            || self.endpoint.contains(['@', '?', '#'])
            || self.endpoint.bytes().any(|b| b.is_ascii_control())
            || expiry <= now
            || !field_matches
            || !self.output_bound_includes_reasoning
            || self.maximum_input_tokens == 0
            || self.maximum_output_tokens == 0
            || self.maximum_wire_bytes == 0
            || self.maximum_wire_bytes > 1_048_576
            || self
                .maximum_price
                .as_ref()
                .is_some_and(|price| !price.valid())
        {
            return Err(BudgetError::Contract);
        }
        let tokens = self
            .maximum_input_tokens
            .checked_add(self.maximum_output_tokens)
            .ok_or(BudgetError::Overflow)?;
        if tokens > MAX_LEDGER_INTEGER {
            return Err(BudgetError::Overflow);
        }
        if let Some(price) = &self.maximum_price
            && price.price(self.maximum_input_tokens, self.maximum_output_tokens)?
                > MAX_LEDGER_INTEGER
        {
            return Err(BudgetError::Overflow);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Quote {
    pub contract: ModelContract,
    pub output_tokens: u64,
    pub maximum: Amounts,
    pub price_covered: bool,
}

impl Quote {
    pub fn validate(&self, now: i64, money_required: bool) -> Result<(), BudgetError> {
        self.contract.validate(now)?;
        if self.output_tokens == 0
            || self.output_tokens > self.contract.maximum_output_tokens
            || self.price_covered != self.contract.maximum_price.is_some()
            || (money_required && !self.price_covered)
        {
            return Err(BudgetError::Contract);
        }
        let expected = Amounts {
            tokens: self
                .contract
                .maximum_input_tokens
                .checked_add(self.output_tokens)
                .ok_or(BudgetError::Overflow)?,
            usd_micros: self
                .contract
                .maximum_price
                .as_ref()
                .map(|price| price.price(self.contract.maximum_input_tokens, self.output_tokens))
                .transpose()?
                .unwrap_or(0),
        };
        if expected != self.maximum {
            return Err(BudgetError::Contract);
        }
        Ok(())
    }

    pub fn usage(&self, usage: &Usage) -> Result<Amounts, BudgetError> {
        if usage.input_tokens > self.contract.maximum_input_tokens
            || usage.output_tokens > self.output_tokens
            || usage
                .cached_input_tokens
                .checked_add(usage.cache_creation_input_tokens)
                .is_none_or(|cached| cached > usage.input_tokens)
            || usage.reasoning_output_tokens > usage.output_tokens
        {
            return Err(BudgetError::Breach);
        }
        Ok(Amounts {
            tokens: usage
                .input_tokens
                .checked_add(usage.output_tokens)
                .ok_or(BudgetError::Overflow)?,
            usd_micros: self
                .contract
                .maximum_price
                .as_ref()
                .map(|price| price.price(usage.input_tokens, usage.output_tokens))
                .transpose()?
                .unwrap_or(0),
        })
    }
}

/// Exact routed provider identity for accounting without a spending contract.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingRoute {
    pub provider_id: String,
    pub endpoint: String,
    pub model: String,
    pub operation: Operation,
}

impl RecordingRoute {
    pub fn validate(&self) -> Result<(), BudgetError> {
        let endpoint = reqwest::Url::parse(&self.endpoint).map_err(|_| BudgetError::Contract)?;
        let valid_component = |value: &str| {
            !value.is_empty()
                && value.len() <= 253
                && value.trim() == value
                && !value.chars().any(char::is_control)
        };
        if !valid_component(&self.provider_id)
            || !valid_component(&self.model)
            || self.endpoint.len() > 2048
            || !(self.endpoint.starts_with("https://") || self.endpoint.starts_with("http://"))
            || self.endpoint.chars().any(char::is_whitespace)
            || self.endpoint.chars().any(char::is_control)
            || self.endpoint.contains(['@', '?', '#', '\\'])
            || !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(BudgetError::Contract);
        }
        Ok(())
    }
}

/// Bounded quotes keep their original persisted JSON representation.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(untagged, deny_unknown_fields)]
pub enum AttemptQuote {
    Bounded(Quote),
    RecordOnly {
        #[serde(rename = "recordOnly")]
        record_only: RecordingRoute,
    },
}

impl From<Quote> for AttemptQuote {
    fn from(quote: Quote) -> Self {
        Self::Bounded(quote)
    }
}

impl AttemptQuote {
    pub fn is_record_only(&self) -> bool {
        matches!(self, Self::RecordOnly { .. })
    }

    pub fn bounded(&self) -> Option<&Quote> {
        match self {
            Self::Bounded(quote) => Some(quote),
            Self::RecordOnly { .. } => None,
        }
    }

    /// Zero for record-only means no reservation, not free or known-zero usage.
    pub fn maximum(&self) -> Amounts {
        self.bounded()
            .map_or(Amounts::default(), |quote| quote.maximum)
    }

    pub fn price_covered(&self) -> bool {
        self.bounded().is_some_and(|quote| quote.price_covered)
    }

    pub fn validate(&self, now: i64, money_required: bool) -> Result<(), BudgetError> {
        match self {
            Self::Bounded(quote) => quote.validate(now, money_required),
            Self::RecordOnly { record_only } => {
                if money_required {
                    return Err(BudgetError::Contract);
                }
                record_only.validate()
            }
        }
    }

    pub fn usage(&self, usage: &Usage) -> Result<Amounts, BudgetError> {
        if let Self::Bounded(quote) = self {
            return quote.usage(usage);
        }
        if usage
            .cached_input_tokens
            .checked_add(usage.cache_creation_input_tokens)
            .is_none_or(|cached| cached > usage.input_tokens)
            || usage.reasoning_output_tokens > usage.output_tokens
        {
            return Err(BudgetError::Breach);
        }
        // Provider-reported tokens are actual usage. Money is explicitly unpriced.
        Amounts {
            tokens: usage.input_tokens,
            usd_micros: 0,
        }
        .checked_add(Amounts {
            tokens: usage.output_tokens,
            usd_micros: 0,
        })
    }

    pub fn expires_at(&self, now: i64) -> Result<i64, BudgetError> {
        let expiry = now
            .checked_add(RESERVATION_TTL_SECONDS)
            .ok_or(BudgetError::Overflow)?;
        match self {
            Self::Bounded(quote) => Ok(expiry.min(
                chrono::DateTime::parse_from_rfc3339(&quote.contract.valid_until)
                    .map_err(|_| BudgetError::Contract)?
                    .timestamp(),
            )),
            Self::RecordOnly { .. } => Ok(expiry),
        }
    }
}

#[cfg(test)]
#[path = "tariff_tests.rs"]
mod tests;
