// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn fixture(required: bool) -> (ExecutionPhase, Value) {
    let phase = serde_json::from_value(json!({
        "name":"research", "objective":"Read and compare the reviewed public pricing sources.",
        "capabilities":["filesystem-read","filesystem-write","network"],
        "minToolCalls":0, "maxToolCalls":5, "freshContext":true,
        "requiredToolCalls":if required { json!([
            {"name":"http_fetch","arguments":{"method":"GET","url":"https://example.com/pricing"}},
            {"name":"http_fetch","arguments":{"method":"GET","url":"https://example.org/pricing"}}
        ]) } else { json!([]) }
    }))
    .unwrap();
    let evidence = json!({"model":"reviewed-model","rounds":1,
        "usage":{"promptTokens":10,"completionTokens":10,"totalTokens":20},
        "phase":{"name":"research","minToolCalls":0,"maxToolCalls":5,
            "attemptedToolCalls":4,"successfulToolCalls":4}});
    (phase, evidence)
}

#[test]
fn terminal_success_requires_all_reviewed_calls_independently_of_minimum() {
    let (phase, mut evidence) = fixture(true);
    assert!(measured(&evidence, &phase, false).is_ok());
    assert!(measured(&evidence, &phase, true).is_err());
    evidence["phase"]["completedRequiredToolCalls"] = json!(1);
    assert!(measured(&evidence, &phase, false).is_ok());
    assert!(measured(&evidence, &phase, true).is_err());
    evidence["phase"]["completedRequiredToolCalls"] = json!(2);
    assert!(measured(&evidence, &phase, true).is_ok());
    evidence["phase"]["successfulToolCalls"] = json!(1);
    assert!(measured(&evidence, &phase, false).is_err());
}

#[test]
fn invalid_or_excessive_required_counts_are_rejected() {
    let (phase, mut evidence) = fixture(true);
    for count in [
        json!(null),
        json!(true),
        json!("2"),
        json!(-1),
        json!(1.5),
        json!(3),
        json!(9_007_199_254_740_992_u64),
    ] {
        evidence["phase"]["completedRequiredToolCalls"] = count.clone();
        assert!(measured(&evidence, &phase, false).is_err(), "{count}");
    }
}

#[test]
fn requirement_free_legacy_evidence_remains_valid_without_counter() {
    let (phase, mut evidence) = fixture(false);
    assert!(measured(&evidence, &phase, true).is_ok());
    evidence["phase"]["completedRequiredToolCalls"] = json!(0);
    assert!(measured(&evidence, &phase, true).is_ok());
    evidence["phase"]["completedRequiredToolCalls"] = json!(1);
    assert!(measured(&evidence, &phase, false).is_err());
}

#[test]
fn required_completion_never_regresses_or_disappears() {
    assert!(required_progress(&json!({}), &json!({"completedRequiredToolCalls":0})).is_ok());
    assert!(required_progress(&json!({"completedRequiredToolCalls":0}), &json!({})).is_ok());
    let previous = json!({"completedRequiredToolCalls":1});
    for current in [
        json!({}),
        json!({"completedRequiredToolCalls":0}),
        json!({"completedRequiredToolCalls":null}),
    ] {
        assert!(required_progress(&previous, &current).is_err());
    }
    assert!(required_progress(&previous, &previous).is_ok());
    assert!(required_progress(&previous, &json!({"completedRequiredToolCalls":2})).is_ok());
}
