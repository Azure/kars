// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};

#[test]
fn record_only_normalization_binds_model_without_fabricating_a_spending_cap() {
    let (bytes, quote) = crate::inference_budget_dispatch::normalize_record_only(
        "provider",
        "https://models.example",
        "model",
        Operation::Responses,
        br#"{"input":"text"}"#,
        1,
    )
    .unwrap();
    assert!(quote.is_record_only());
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({"input":"text", "model":"model"})
    );
}

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
fn chat_tool_followup_accepts_request_fields_not_provider_response_metadata() {
    let mut body = json!({
        "model": "model-revision-1",
        "messages": [
            {"role": "user", "content": "Write a briefing"},
            {"role": "assistant", "content": null, "refusal": null, "tool_calls": [
                {"id": "call-1", "type": "function", "function": {"name": "mesh_inbox", "arguments": "{}"}}
            ]},
            {"role": "tool", "tool_call_id": "call-1", "content": "No messages"}
        ]
    });
    for content in [Value::Null, json!("Checking the inbox")] {
        body["messages"][1]["content"] = content;
        let (wire, quote) = contract()
            .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
            .unwrap();
        let normalized: Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(normalized["messages"], body["messages"]);
        assert_eq!(normalized["max_completion_tokens"], 20);
        assert_eq!(quote.maximum.tokens, 30);
    }
    for field in ["annotations", "provider_metadata"] {
        let mut response_reused = body.clone();
        response_reused["messages"][1][field] = json!([]);
        assert!(
            contract()
                .normalize(&serde_json::to_vec(&response_reused).unwrap(), 100, true)
                .is_err()
        );
    }
    body["messages"][1]
        .as_object_mut()
        .unwrap()
        .remove("content");
    assert!(
        contract()
            .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
            .is_err()
    );
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
fn chat_output_alias_preserves_explicit_cap_and_quote_for_loopback_clients() {
    for (field, canonical, alias) in [
        (
            OutputField::Completion,
            "max_completion_tokens",
            "max_tokens",
        ),
        (OutputField::Tokens, "max_tokens", "max_completion_tokens"),
    ] {
        let mut contract = contract();
        contract.output_field = field;
        for cap in [1, 12, 20] {
            let mut body: Value = serde_json::from_slice(&request()).unwrap();
            body[alias] = json!(cap);
            let (wire, quote) = contract
                .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
                .unwrap();
            let normalized: Value = serde_json::from_slice(&wire).unwrap();
            assert_eq!(normalized[canonical], cap);
            assert!(normalized.get(alias).is_none());
            assert_eq!(normalized["messages"], body["messages"]);
            assert_eq!(quote.output_tokens, cap);
            assert_eq!(quote.maximum.tokens, 10 + cap);
            body.as_object_mut().unwrap().remove(alias);
            body[canonical] = json!(cap);
            let (canonical_wire, canonical_quote) = contract
                .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
                .unwrap();
            assert_eq!(wire, canonical_wire);
            assert_eq!(quote.maximum, canonical_quote.maximum);
        }
    }
}

#[test]
fn chat_output_alias_rejects_conflicts_invalid_caps_and_wrong_operation() {
    for (field, canonical, alias) in [
        (
            OutputField::Completion,
            "max_completion_tokens",
            "max_tokens",
        ),
        (OutputField::Tokens, "max_tokens", "max_completion_tokens"),
    ] {
        let mut contract = contract();
        contract.output_field = field;
        for cap in [
            json!(0),
            json!(-1),
            json!(21),
            json!(1.5),
            json!("12"),
            json!(true),
            Value::Null,
        ] {
            let mut body: Value = serde_json::from_slice(&request()).unwrap();
            body[alias] = cap;
            assert!(
                contract
                    .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
                    .is_err()
            );
        }
        for other in [alias, "max_output_tokens"] {
            for cap in [12, 20] {
                let mut body: Value = serde_json::from_slice(&request()).unwrap();
                body[canonical] = json!(12);
                body[other] = json!(cap);
                assert!(
                    contract
                        .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
                        .is_err()
                );
            }
        }
    }
    for (operation, field, body) in [
        (
            Operation::AnthropicMessages,
            OutputField::Tokens,
            json!({"messages": [{"role": "user", "content": "hello"}], "max_completion_tokens": 12}),
        ),
        (
            Operation::Responses,
            OutputField::Output,
            json!({"input": "hello", "max_tokens": 12}),
        ),
    ] {
        let mut contract = contract();
        contract.operation = operation;
        contract.output_field = field;
        assert!(
            contract
                .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
                .is_err()
        );
    }
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
        json!({"max_tokens": 20, "max_completion_tokens": 20}),
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
fn responses_native_hermes_cache_and_reasoning_hints_preserve_wire_and_bounds() {
    let mut contract = contract();
    contract.operation = Operation::Responses;
    contract.output_field = OutputField::Output;
    // Hermes f97608f1 ResponsesApiTransport emits both hints on azure-foundry.
    let mut body = json!({
        "model": contract.model,
        "instructions": "Inspect cluster health using the approved read-only tools.",
        "input": [{"role": "user", "content": [{"type": "input_text", "text": "Inspect health"}]}],
        "tools": [{"type": "function", "name": "sre_inventory", "strict": false,
            "parameters": {"type": "object", "properties": {}}}],
        "tool_choice": "auto", "parallel_tool_calls": true,
        "reasoning": {"effort": "medium", "summary": "auto"},
        "prompt_cache_key": "pck_0123456789abcdef01234567",
        "include": ["reasoning.encrypted_content"], "store": false,
        "max_output_tokens": 12
    });
    for followup in [false, true] {
        if followup {
            // Native Foundry post-tool replay suppresses encrypted reasoning.
            body["include"] = json!([]);
            body["input"].as_array_mut().unwrap().extend([
                json!({"type": "function_call", "id": "fc_1", "call_id": "call_1",
                    "name": "sre_inventory", "arguments": "{}", "status": "completed"}),
                json!({"type": "function_call_output", "call_id": "call_1", "output": "Healthy"}),
            ]);
        }
        let (wire, quote) = contract
            .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
            .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&wire).unwrap(), body);
        assert_eq!(quote.maximum.tokens, 22);
        assert_eq!(quote.maximum.usd_micros, 39);
        assert_eq!(quote.output_tokens, 12);
    }
    body.as_object_mut().unwrap().remove("max_output_tokens");
    let (wire, quote) = contract
        .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&wire).unwrap()["max_output_tokens"],
        20
    );
    assert_eq!(quote.maximum.tokens, 30);
    contract.maximum_wire_bytes = 32;
    assert!(
        contract
            .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
            .is_err()
    );
}

#[test]
fn responses_assistant_phase_replay_preserves_history_and_accounting_bounds() {
    let mut contract = contract();
    contract.operation = Operation::Responses;
    contract.output_field = OutputField::Output;
    let mut body = json!({
        "model": contract.model, "store": false, "max_output_tokens": 12,
        "input": [
            {"role": "user", "content": "Fetch the approved page"},
            {"type": "message", "role": "assistant", "id": "msg_1", "status": "completed",
                "phase": "final_answer", "content": [{"type": "output_text", "text": "HTTP 200"}]},
            {"role": "user", "content": "Fetch it once more"}
        ]
    });
    for phase in ["commentary", "final_answer"] {
        body["input"][1]["phase"] = json!(phase);
        let (wire, quote) = contract
            .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
            .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&wire).unwrap(), body);
        assert_eq!(quote.maximum.tokens, 22);
        assert_eq!(quote.maximum.usd_micros, 39);
        assert_eq!(quote.output_tokens, 12);
    }
    for invalid in [
        Value::Null,
        json!(""),
        json!("analysis"),
        json!(true),
        json!({"type": "web_search"}),
    ] {
        let mut rejected = body.clone();
        rejected["input"][1]["phase"] = invalid;
        assert!(
            contract
                .normalize(&serde_json::to_vec(&rejected).unwrap(), 100, true)
                .is_err()
        );
    }
    for role in [Value::Null, json!("user"), json!("system"), json!("tool")] {
        let mut rejected = body.clone();
        rejected["input"][1]["role"] = role;
        assert!(
            contract
                .normalize(&serde_json::to_vec(&rejected).unwrap(), 100, true)
                .is_err()
        );
    }
    for extra in [
        json!({"previous_response_id": "resp_1"}),
        json!({"provider_metadata": {}}),
    ] {
        let mut rejected = body.clone();
        rejected["input"][1]
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            contract
                .normalize(&serde_json::to_vec(&rejected).unwrap(), 100, true)
                .is_err()
        );
    }
    body["max_output_tokens"] = json!(21);
    assert!(
        contract
            .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
            .is_err()
    );
    body["max_output_tokens"] = json!(12);
    contract.maximum_wire_bytes = serde_json::to_vec(&body).unwrap().len() as u64 - 1;
    assert!(
        contract
            .normalize(&serde_json::to_vec(&body).unwrap(), 100, true)
            .is_err()
    );
}

#[test]
fn responses_hints_do_not_admit_unbounded_or_provider_stateful_operations() {
    let mut contract = contract();
    contract.operation = Operation::Responses;
    contract.output_field = OutputField::Output;
    let body = json!({"model": contract.model, "input": "Inspect health", "store": false,
        "include": ["reasoning.encrypted_content"], "prompt_cache_key": "sre-sweep"});
    for extra in [
        json!({"include": null}),
        json!({"include": "reasoning.encrypted_content"}),
        json!({"include": [null]}),
        json!({"include": ["reasoning.encrypted_content", "reasoning.encrypted_content"]}),
        json!({"include": ["web_search_call.action.sources"]}),
        json!({"include": ["file_search_call.results"]}),
        json!({"prompt_cache_key": null}),
        json!({"prompt_cache_key": {"previous_response_id": "resp_1"}}),
        json!({"prompt_cache_key": ""}),
        json!({"prompt_cache_key": "  "}),
        json!({"prompt_cache_key": "x".repeat(65)}),
        json!({"prompt_cache_key": "é".repeat(33)}),
        json!({"prompt_cache_retention": "24h"}),
        json!({"previous_response_id": "resp_1"}),
        json!({"conversation": "conv_1"}),
        json!({"context_management": [{"type": "compaction", "compact_threshold": 1000}]}),
        json!({"store": true}),
        json!({"background": true}),
        json!({"tools": [{"type": "web_search"}]}),
        json!({"input": [{"type": "reasoning", "encrypted_content": "opaque", "summary": []}]}),
        json!({"max_output_tokens": 21}),
        json!({"max_output_tokens": 0}),
        json!({"max_output_tokens": "20"}),
        json!({"max_completion_tokens": 20}),
    ] {
        let mut invalid = body.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            contract
                .normalize(&serde_json::to_vec(&invalid).unwrap(), 100, true)
                .is_err(),
            "{extra}"
        );
    }
    let mut boundary = body.clone();
    boundary["prompt_cache_key"] = json!("x".repeat(64));
    assert!(
        contract
            .normalize(&serde_json::to_vec(&boundary).unwrap(), 100, true)
            .is_ok()
    );
    for operation in [Operation::ChatCompletions, Operation::AnthropicMessages] {
        contract.operation = operation;
        contract.output_field = OutputField::Tokens;
        let request = json!({"model": contract.model,
            "messages": [{"role": "user", "content": "hello"}],
            "include": [], "prompt_cache_key": "sre-sweep"});
        assert!(
            contract
                .normalize(&serde_json::to_vec(&request).unwrap(), 100, true)
                .is_err()
        );
    }
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
