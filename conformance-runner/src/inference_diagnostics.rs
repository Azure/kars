// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde_json::Value;

/// Counts only: never copy provider strings, prompt content, or model output into logs.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SafetyObservation {
    prompt_results_present: bool,
    prompt_results: usize,
    complete_shield_results: usize,
    detected_attacks: usize,
    filtered_attacks: usize,
    choices: usize,
    content_filtered_choices: usize,
    structured_refusal_choices: usize,
}

pub(crate) fn summarize(body: &Value) -> SafetyObservation {
    let mut summary = SafetyObservation::default();
    if let Some(results) = body.get("prompt_filter_results").and_then(Value::as_array) {
        summary.prompt_results_present = true;
        summary.prompt_results = results.len();
        for result in results {
            let filters = &result["content_filter_results"];
            let mut complete = false;
            for name in ["jailbreak", "indirect_attack"] {
                let annotation = &filters[name];
                let detected = annotation["detected"].as_bool();
                let filtered = annotation["filtered"].as_bool();
                complete |= detected.is_some() && filtered.is_some();
                summary.detected_attacks += usize::from(detected == Some(true));
                summary.filtered_attacks += usize::from(filtered == Some(true));
            }
            summary.complete_shield_results += usize::from(complete);
        }
    }
    if let Some(choices) = body.get("choices").and_then(Value::as_array) {
        summary.choices = choices.len();
        for choice in choices {
            summary.content_filtered_choices +=
                usize::from(choice["finish_reason"].as_str() == Some("content_filter"));
            summary.structured_refusal_choices += usize::from(
                choice["message"]["refusal"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
            );
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_empty_and_malformed_shields_are_not_complete_evidence() {
        assert!(!summarize(&json!({})).prompt_results_present);
        let empty = summarize(&json!({"prompt_filter_results": []}));
        assert!(empty.prompt_results_present);
        assert_eq!(empty.prompt_results, 0);
        for annotation in [
            json!(null),
            json!({}),
            json!({"detected": false}),
            json!({"detected": "false", "filtered": false}),
        ] {
            let summary = summarize(&json!({"prompt_filter_results": [
                {"content_filter_results": {"jailbreak": annotation}}
            ]}));
            assert_eq!(summary.prompt_results, 1);
            assert_eq!(summary.complete_shield_results, 0);
        }
    }

    #[test]
    fn valid_negative_detection_is_distinct_from_missing_evidence() {
        let summary = summarize(&json!({"prompt_filter_results": [
            {"content_filter_results": {"jailbreak": {"detected": false, "filtered": false}}}
        ]}));
        assert_eq!(summary.complete_shield_results, 1);
        assert_eq!(summary.detected_attacks, 0);
        assert_eq!(summary.filtered_attacks, 0);
    }

    #[test]
    fn detected_and_filtered_counts_are_independent_and_cover_every_prompt() {
        let summary = summarize(&json!({"prompt_filter_results": [
            {"content_filter_results": {
                "jailbreak": {"detected": true, "filtered": false},
                "indirect_attack": {"detected": true, "filtered": true}
            }},
            {"content_filter_results": {"jailbreak": {"detected": false, "filtered": true}}},
            {"content_filter_results": {"jailbreak": {"detected": true}}}
        ]}));
        assert_eq!(summary.prompt_results, 3);
        assert_eq!(summary.complete_shield_results, 2);
        assert_eq!(summary.detected_attacks, 3);
        assert_eq!(summary.filtered_attacks, 2);
    }

    #[test]
    fn diagnostics_never_copy_provider_strings_or_model_output() {
        let body = json!({
            "prompt_filter_results": [{"content_filter_results": {
                "jailbreak": {"detected": "secret-marker", "filtered": "secret-marker"},
                "secret-marker": {"detected": true, "filtered": true}
            }}],
            "choices": [
                {"finish_reason": "content_filter", "message": {"content": "secret-marker", "refusal": "secret-marker"}},
                {"finish_reason": "secret-marker", "message": {"content": "secret-marker", "refusal": null}},
                {"message": {"refusal": ""}}
            ],
            "api_key": "secret-marker",
            "reason": "secret-marker"
        });
        let summary = summarize(&body);
        assert_eq!(summary.choices, 3);
        assert_eq!(summary.content_filtered_choices, 1);
        assert_eq!(summary.structured_refusal_choices, 1);
        assert!(!format!("{summary:?}").contains("secret-marker"));
        assert_eq!(summary.detected_attacks, 0);
    }

    #[test]
    fn non_object_entries_are_safe_and_do_not_count_as_evidence() {
        for body in [json!(null), json!(false), json!([]), json!("ignored")] {
            assert_eq!(summarize(&body), SafetyObservation::default());
        }
        let summary =
            summarize(&json!({"prompt_filter_results": [null, [], 3], "choices": [false]}));
        assert_eq!(summary.prompt_results, 3);
        assert_eq!(summary.complete_shield_results, 0);
        assert_eq!(summary.choices, 1);
    }
}
