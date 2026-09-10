// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};

pub(super) fn contract() -> ModelContract {
    ModelContract {
        id: "text-model".into(),
        version: "operator-v1".into(),
        valid_until: "2030-01-01T00:00:00Z".into(),
        provider_id: "named-provider".into(),
        endpoint: "https://operator.example/inference".into(),
        model: "model-revision-1".into(),
        operation: Operation::ChatCompletions,
        output_field: OutputField::Completion,
        maximum_input_tokens: 10,
        maximum_output_tokens: 20,
        maximum_wire_bytes: 4096,
        output_bound_includes_reasoning: true,
        maximum_price: Some(MaximumPrice::TokenRates {
            input_micros_per_million: 1_000_001,
            output_micros_per_million: 2_000_001,
            fixed_micros: 3,
        }),
    }
}

fn request() -> Vec<u8> {
    serde_json::to_vec(
        &json!({"model": "model-revision-1", "messages": [{"role": "user", "content": "hello"}]}),
    )
    .unwrap()
}

#[test]
fn output_field_wire_names_and_schema_remain_operator_compatible() {
    let variants = [
        (OutputField::Tokens, "MaxTokens"),
        (OutputField::Completion, "MaxCompletionTokens"),
        (OutputField::Output, "MaxOutputTokens"),
    ];
    for (variant, wire) in variants {
        assert_eq!(serde_json::to_value(variant).unwrap(), json!(wire));
        assert_eq!(
            serde_json::from_value::<OutputField>(json!(wire)).unwrap(),
            variant
        );
    }
    let schema = serde_json::to_value(schemars::schema_for!(OutputField)).unwrap();
    assert_eq!(
        schema["enum"],
        json!(["MaxTokens", "MaxCompletionTokens", "MaxOutputTokens"])
    );
    for internal_name in ["Tokens", "Completion", "Output"] {
        assert!(serde_json::from_value::<OutputField>(json!(internal_name)).is_err());
    }
}

#[test]
fn missing_maximum_is_injected_and_input_uses_operator_bound_not_character_estimate() {
    let (wire, quote) = contract().normalize(&request(), 100, true).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&wire).unwrap()["max_completion_tokens"],
        20
    );
    assert_eq!(quote.maximum.tokens, 30);
    assert_eq!(quote.maximum.usd_micros, 55);
    assert!(quote.price_covered);
}

#[test]
fn no_price_is_allowed_only_when_no_monetary_axis_requires_it() {
    let mut contract = contract();
    contract.maximum_price = None;
    assert!(contract.normalize(&request(), 100, true).is_err());
    let (_, quote) = contract.normalize(&request(), 100, false).unwrap();
    assert!(!quote.price_covered);
    assert_eq!(quote.maximum.tokens, 30);
}

#[test]
fn expiry_unknown_bounds_or_uncovered_reasoning_are_not_zero_cost_fallbacks() {
    for case in [
        "expired",
        "no-input",
        "no-output",
        "reasoning",
        "zero-price",
    ] {
        let mut contract = contract();
        match case {
            "expired" => contract.valid_until = "1970-01-01T00:00:01Z".into(),
            "no-input" => contract.maximum_input_tokens = 0,
            "no-output" => contract.maximum_output_tokens = 0,
            "reasoning" => contract.output_bound_includes_reasoning = false,
            _ => contract.maximum_price = Some(MaximumPrice::PerRequest { maximum_micros: 0 }),
        }
        assert!(contract.normalize(&request(), 100, true).is_err(), "{case}");
    }
}

#[test]
fn money_math_is_checked_and_rounds_each_category_up() {
    let price = MaximumPrice::TokenRates {
        input_micros_per_million: 1,
        output_micros_per_million: 1,
        fixed_micros: 0,
    };
    assert_eq!(price.price(1, 1).unwrap(), 2);
    let price = MaximumPrice::TokenRates {
        input_micros_per_million: u64::MAX,
        output_micros_per_million: u64::MAX,
        fixed_micros: u64::MAX,
    };
    assert!(price.price(u64::MAX, u64::MAX).is_err());
}

#[test]
fn unsupported_async_multimodal_server_tools_and_multiplicity_fail_closed() {
    for extra in [
        json!({"n": 2}),
        json!({"background": true}),
        json!({"store": true}),
        json!({"web_search_options": {}}),
        json!({"tools": [{"type": "web_search"}]}),
        json!({"messages": [{"role": "user", "content": [{"type": "image_url", "image_url": {"url": "https://example.test/a"}}]}]}),
        json!({"max_completion_tokens": 21}),
        json!({"max_completion_tokens": "20"}),
        json!({"max_tokens": 20}),
    ] {
        let mut value: Value = serde_json::from_slice(&request()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            contract()
                .normalize(&serde_json::to_vec(&value).unwrap(), 100, true)
                .is_err()
        );
    }
}

#[test]
fn client_function_tools_remain_available_without_enabling_hosted_generation() {
    let mut value: Value = serde_json::from_slice(&request()).unwrap();
    value["tools"] = json!([{"type": "function", "function": {"name": "read_file", "parameters": {"type": "object"}}}]);
    assert!(
        contract()
            .normalize(&serde_json::to_vec(&value).unwrap(), 100, true)
            .is_ok()
    );
}

#[test]
fn native_and_responses_shapes_have_explicit_distinct_output_contracts() {
    let mut contract = contract();
    contract.operation = Operation::AnthropicMessages;
    contract.output_field = OutputField::Tokens;
    let body = json!({"model": contract.model, "messages": [{"role": "user", "content": "hello"}],
        "tools": [{"name": "read_file", "input_schema": {"type": "object"}}]});
    let (wire, _) = contract
        .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&wire).unwrap()["max_tokens"],
        20
    );
    contract.operation = Operation::Responses;
    contract.output_field = OutputField::Output;
    let body =
        json!({"model": contract.model, "input": "hello", "store": false, "background": false});
    let (wire, _) = contract
        .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&wire).unwrap()["max_output_tokens"],
        20
    );
}

#[test]
fn trustworthy_usage_is_bounded_and_cache_reasoning_are_subsets_not_extra_refunds() {
    let (_, quote) = contract().normalize(&request(), 100, true).unwrap();
    let usage = Usage {
        input_tokens: 3,
        output_tokens: 5,
        cached_input_tokens: 1,
        cache_creation_input_tokens: 1,
        reasoning_output_tokens: 2,
    };
    assert_eq!(quote.usage(&usage).unwrap().tokens, 8);
    assert!(
        quote
            .usage(&Usage {
                input_tokens: 11,
                ..usage.clone()
            })
            .is_err()
    );
    assert!(
        quote
            .usage(&Usage {
                output_tokens: 21,
                ..usage.clone()
            })
            .is_err()
    );
    assert!(
        quote
            .usage(&Usage {
                cached_input_tokens: 3,
                ..usage.clone()
            })
            .is_err()
    );
    assert!(
        quote
            .usage(&Usage {
                reasoning_output_tokens: 6,
                ..usage
            })
            .is_err()
    );
}
