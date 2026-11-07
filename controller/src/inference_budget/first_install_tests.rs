// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[test]
fn adopted_policy_updates_preserve_all_other_fields() {
    let original = json!({
        "appliesTo": {"sandboxName":"sre"},
        "tokenBudget": {"dailyTokens":1500000,"perRequestTokens":528000},
        "modelPreference": {"primary":{"provider":"azure-openai","deployment":"model"},"fallback":[]},
        "contentSafety": {"requirePromptShields":true}
    });
    assert!(verify_adopted_policy_update(&original, &original, false).is_ok());
    for daily in [1500000, 5000000] {
        let mut selected = original.clone();
        selected["tokenBudget"]["dailyTokens"] = json!(daily);
        selected["modelPreference"]["primary"] = json!({
            "provider":"github-copilot", "deployment":"claude-opus-5.5"
        });
        let before = selected.clone();
        assert!(verify_adopted_policy_update(&selected, &original, false).is_ok());
        assert_eq!(selected, before);
        for value in [
            json!(0),
            json!(-1),
            json!(1499999),
            json!(1.5),
            json!("5000000"),
            Value::Null,
            json!(9007199254740992_u64),
        ] {
            let mut changed = selected.clone();
            changed["tokenBudget"]["dailyTokens"] = value;
            assert!(verify_adopted_policy_update(&changed, &original, false).is_err());
        }
        for (path, value) in [
            ("/tokenBudget/perRequestTokens", json!(0)),
            ("/appliesTo/sandboxName", json!("other")),
            (
                "/modelPreference/fallback",
                json!([{"provider":"other","deployment":"other"}]),
            ),
            ("/contentSafety/requirePromptShields", json!(false)),
        ] {
            let mut changed = selected.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(verify_adopted_policy_update(&changed, &original, false).is_err());
        }
        for primary in [
            json!({"provider":"", "deployment":"model"}),
            json!({"provider":"bad/tag", "deployment":"model"}),
            json!({"provider":"a".repeat(65), "deployment":"model"}),
            json!({"provider":"github-copilot", "deployment":" "}),
            json!({"provider":"github-copilot", "deployment":"line\nbreak"}),
            json!({"provider":"github-copilot", "deployment":"a".repeat(257)}),
            json!({"provider":"github-copilot"}),
            json!({"provider":"github-copilot", "deployment":"model", "extra":true}),
            Value::Null,
        ] {
            let mut changed = selected.clone();
            changed["modelPreference"]["primary"] = primary;
            assert!(verify_adopted_policy_update(&changed, &original, false).is_err());
        }
        selected["tokenBudget"]["monthlyTokens"] = json!(0);
        assert!(verify_adopted_policy_update(&selected, &original, false).is_err());
        let mut unlimited = original.clone();
        unlimited["tokenBudget"]["dailyTokens"] = json!(0);
        assert!(verify_adopted_policy_update(&before, &unlimited, false).is_err());
    }
    let mut daily_only = original.clone();
    daily_only["tokenBudget"]["dailyTokens"] = json!(5000000);
    assert!(verify_adopted_policy_update(&daily_only, &original, false).is_ok());
}

#[test]
fn adopted_unbounded_funding_allows_only_removing_token_ceilings() {
    let original = json!({
        "appliesTo": {"sandboxName":"sre"},
        "tokenBudget": {"dailyTokens":1500000,"monthlyTokens":9000000,"perRequestTokens":528000},
        "modelPreference": {"primary":{"provider":"azure-foundry","deployment":"model"},"fallback":[]},
        "contentSafety": {"requirePromptShields":true}
    });
    for mask in 1..8 {
        let mut selected = original.clone();
        for (i, field) in ["dailyTokens", "monthlyTokens", "perRequestTokens"]
            .iter()
            .enumerate()
        {
            if mask & (1 << i) != 0 {
                selected["tokenBudget"]
                    .as_object_mut()
                    .unwrap()
                    .remove(*field);
            }
        }
        assert!(verify_adopted_policy_update(&selected, &original, true).is_ok());
        assert!(verify_adopted_policy_update(&selected, &original, false).is_err());
        for (path, value) in [
            ("/appliesTo/sandboxName", json!("other")),
            ("/contentSafety/requirePromptShields", json!(false)),
            (
                "/modelPreference/fallback",
                json!([{"provider":"other","deployment":"other"}]),
            ),
        ] {
            let mut changed = selected.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(verify_adopted_policy_update(&changed, &original, true).is_err());
        }
        selected["tokenBudget"]["unknown"] = json!(true);
        assert!(verify_adopted_policy_update(&selected, &original, true).is_err());
    }
    let mut uncapped = original.clone();
    uncapped.as_object_mut().unwrap().remove("tokenBudget");
    assert!(verify_adopted_policy_update(&uncapped, &original, true).is_ok());
    assert!(verify_adopted_policy_update(&uncapped, &original, false).is_err());
    for unbounded in [false, true] {
        assert!(verify_adopted_policy_update(&uncapped, &uncapped, unbounded).is_ok());
    }
    for value in [json!(0), json!(-1), json!(1.5), json!("528000")] {
        let mut selected = original.clone();
        selected["tokenBudget"]["perRequestTokens"] = value;
        assert!(verify_adopted_policy_update(&selected, &original, true).is_err());
    }
}

#[test]
fn adopted_unbounded_cap_removal_survives_typed_policy_serialization() {
    let typed = |spec: Value| {
        let spec: crate::inference_policy::InferencePolicySpec =
            serde_json::from_value(spec).unwrap();
        serde_json::to_value(spec).unwrap()
    };
    for budget in [
        json!({"dailyTokens":30000000,"perRequestTokens":528000}),
        json!({"perRequestTokens":528000}),
    ] {
        let original = json!({
            "appliesTo":{"sandboxName":"sre"},
            "tokenBudget":budget,
            "modelPreference":{"primary":{"provider":"azure-openai","deployment":"model"},"fallback":[]},
            "contentSafety":{"requirePromptShields":true}
        });
        let original = typed(original);
        assert!(verify_adopted_policy_update(&original, &original, false).is_ok());
        for mask in 1..8 {
            let mut selected = original.clone();
            for (i, field) in ["dailyTokens", "monthlyTokens", "perRequestTokens"]
                .iter()
                .enumerate()
            {
                if mask & (1 << i) != 0 {
                    selected["tokenBudget"]
                        .as_object_mut()
                        .unwrap()
                        .remove(*field);
                }
            }
            selected["modelPreference"]["primary"] = json!({
                "provider":"github-copilot", "deployment":"claude-opus-5.5"
            });
            let selected = typed(selected);
            let before = selected.clone();
            assert!(verify_adopted_policy_update(&selected, &original, true).is_ok());
            assert_eq!(selected, before);
            let removed = selected["tokenBudget"] != original["tokenBudget"];
            assert_eq!(
                verify_adopted_policy_update(&selected, &original, false).is_err(),
                removed
            );
            let mut drift = selected.clone();
            drift["contentSafety"]["requirePromptShields"] = json!(false);
            assert!(verify_adopted_policy_update(&drift, &original, true).is_err());
        }
        let mut selected = original.clone();
        selected.as_object_mut().unwrap().remove("tokenBudget");
        let selected = typed(selected);
        assert!(verify_adopted_policy_update(&selected, &original, true).is_ok());
        assert!(verify_adopted_policy_update(&selected, &original, false).is_err());
        assert!(verify_adopted_policy_update(&selected, &selected, true).is_ok());
    }
}

fn rendered_source() -> (Value, Value) {
    // These blocks contain no Helm expressions: read the maintained renderer
    // and CRD rather than duplicating their security defaults in a fixture.
    let template = include_str!("../../../deploy/helm/kars/templates/_sre-resources.tpl");
    let sandbox: Value = serde_saphyr::from_str(
        template
            .split_once("  sandbox:\n")
            .unwrap()
            .1
            .split_once("\n  inferenceRef:")
            .unwrap()
            .0,
    )
    .unwrap();
    let crd = include_str!("../../../deploy/helm/kars/templates/crd.yaml");
    let schema: Value = serde_saphyr::from_str(
        crd.split_once("                sandbox:\n")
            .unwrap()
            .1
            .split_once("                inferenceRef:")
            .unwrap()
            .0,
    )
    .unwrap();
    (
        json!({"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
        "metadata":{"name":"sre","namespace":"kars-system"},
        "spec":{"runtime":{"kind":"Hermes","hermes":{"extraEnv":{"SRE_ENABLED":"true"}}},
            "sandbox":sandbox,"inferenceRef":{"name":"sre-inference"}}}),
        schema,
    )
}

fn api_default_sandbox(source: &mut Value, schema: &Value) {
    for (key, property) in schema["properties"].as_object().unwrap() {
        if let Some(default) = property.get("default") {
            source["spec"]["sandbox"]
                .as_object_mut()
                .unwrap()
                .entry(key.clone())
                .or_insert_with(|| default.clone());
        }
    }
}

#[test]
fn rendered_sre_security_survives_actual_crd_defaulting() {
    let (desired, schema) = rendered_source();
    let mut live = desired.clone();
    api_default_sandbox(&mut live, &schema);
    assert_eq!(live, desired);
    let source: KarsSandbox = serde_json::from_value(live).unwrap();
    assert!(validate_source_spec(&source, desired.clone()).is_ok());

    // Reproduce the original partial object and both real API/serde mismatches.
    let mut partial = desired;
    partial["spec"]["sandbox"] = json!({"isolation":"standard"});
    let raw: KarsSandbox = serde_json::from_value(partial.clone()).unwrap();
    let mut defaulted = partial.clone();
    api_default_sandbox(&mut defaulted, &schema);
    let defaulted: KarsSandbox = serde_json::from_value(defaulted).unwrap();
    assert_ne!(
        raw.spec.sandbox.as_ref().unwrap().writable_paths,
        defaulted.spec.sandbox.as_ref().unwrap().writable_paths
    );
    assert_ne!(
        raw.spec.sandbox.as_ref().unwrap().selinux_context,
        defaulted.spec.sandbox.as_ref().unwrap().selinux_context
    );
    assert!(validate_source_spec(&defaulted, partial).is_err());
}

#[test]
fn defaulted_sre_security_mutations_are_not_normalized_away() {
    let (desired, schema) = rendered_source();
    for (path, value) in [
        ("/spec/sandbox/isolation", json!("enhanced")),
        ("/spec/sandbox/seccompProfile", json!("other")),
        ("/spec/sandbox/selinuxContext", json!("other_t")),
        ("/spec/sandbox/readOnlyRootFilesystem", json!(false)),
        ("/spec/sandbox/runAsNonRoot", json!(false)),
        ("/spec/sandbox/allowPrivilegeEscalation", json!(true)),
        (
            "/spec/sandbox/writablePaths",
            json!(["/sandbox", "/tmp", "/etc"]),
        ),
        ("/spec/sandbox/writablePaths", json!(null)),
        ("/spec/inferenceRef/name", json!("other")),
        ("/spec/runtime/kind", json!("OpenClaw")),
    ] {
        let mut live = desired.clone();
        api_default_sandbox(&mut live, &schema);
        *live.pointer_mut(path).unwrap() = value;
        let source: KarsSandbox = serde_json::from_value(live).unwrap();
        assert!(
            validate_source_spec(&source, desired.clone()).is_err(),
            "{path}"
        );
    }
}

pub(super) fn fixture() -> (ConfigMap, KarsSRERegistration) {
    let encoded = json!({"version":1,"release":"kars","namespace":"kars-system",
        "valuesDigest":"a".repeat(64), "funding":{"tokens":1000,"expiresAt":"2999-01-01T00:00:00Z"},
        "budget":{"enabled":true}, "resources":[
            {"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox","metadata":{"name":"sre","namespace":"kars-system"}},
            {"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy","metadata":{"name":"sre-inference","namespace":"kars-system"}},
            {"apiVersion":"kars.azure.com/v1alpha1","kind":"ToolPolicy","metadata":{"name":"sre-tools","namespace":"kars-system"}}
        ]}).to_string();
    let map = serde_json::from_value(json!({"apiVersion":"v1","kind":"ConfigMap","immutable":true,
        "metadata":{"name":NAME,"namespace":"kars-system","uid":"intent-uid","resourceVersion":"1",
            "annotations":{"meta.helm.sh/release-name":"kars","meta.helm.sh/release-namespace":"kars-system"}},
        "data":{"intent.json":encoded}})).unwrap();
    let reg = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
        "metadata":{"name":"canonical","uid":"registration-uid"},"spec":{
            "controller":{"namespace":{"name":"kars-system","uid":"workspace-uid"},
                "deployment":{"name":"kars-controller","uid":"controller-uid"},"release":"kars"},
            "sandbox":{"namespace":"kars-system","name":"sre","uid":"source-uid"},
            "runtimeNamespace":{"name":"kars-sre","uid":"runtime-uid"},
            "firstInstallFunding":{"intent":{"name":NAME,"uid":"intent-uid",
                "digest":crate::providers::signing::sha256_hex(encoded.as_bytes())},
                "grant":{"root":{"kind":"KarsSandbox","resource":{"namespace":"kars-system","name":"sre","uid":"source-uid"},
                    "workspaceUid":"workspace-uid","clusterUid":"cluster-uid"},
                    "limits":{"tokens":1000},"expiresAt":"2999-01-01T00:00:00Z"}}
        }})).unwrap();
    (map, reg)
}

#[test]
fn contract_renewal_preserves_every_sealed_field_and_input() {
    let original = json!({"version":"catalog-v1", "nonInferenceEgressHosts":[], "contracts":[{
        "id":"chat", "version":"v1", "validUntil":"2020-01-01T00:00:00Z",
        "providerId":"azure-openai", "endpoint":"https://models.example", "model":"model",
        "operation":"ChatCompletions", "outputField":"MaxCompletionTokens",
        "maximumInputTokens":400000, "maximumOutputTokens":128000,
        "maximumWireBytes":1048576, "outputBoundIncludesReasoning":true
    }]});
    let mut renewed = original.clone();
    renewed["version"] = json!("catalog-v2");
    renewed["contracts"][0]["version"] = json!("v2");
    renewed["contracts"][0]["validUntil"] = json!("2999-01-01T00:00:00Z");
    let before = renewed.clone();
    let now = 1_800_000_000;
    verify_contract_renewal(&renewed, &original, now).unwrap();
    assert_eq!(renewed, before);
    assert!(verify_contract_renewal(&original, &original, now).is_err());
    for (path, value) in [
        ("/version", json!("catalog-v1")),
        ("/version", json!(null)),
        ("/nonInferenceEgressHosts", json!(["new.example"])),
        ("/contracts/0/id", json!("other")),
        ("/contracts/0/providerId", json!("other")),
        ("/contracts/0/endpoint", json!("https://other.example")),
        ("/contracts/0/model", json!("other")),
        ("/contracts/0/operation", json!("Responses")),
        ("/contracts/0/outputField", json!("MaxTokens")),
        ("/contracts/0/maximumInputTokens", json!(500000)),
        ("/contracts/0/maximumOutputTokens", json!(256000)),
        ("/contracts/0/maximumWireBytes", json!(2097152)),
        ("/contracts/0/outputBoundIncludesReasoning", json!(false)),
        ("/contracts/0/version", json!("v1")),
        ("/contracts/0/version", json!("")),
        ("/contracts/0/version", json!(123)),
    ] {
        let mut changed = renewed.clone();
        *changed.pointer_mut(path).unwrap() = value;
        assert!(
            verify_contract_renewal(&changed, &original, now).is_err(),
            "{path}"
        );
    }
    for expiry in [
        "",
        "2019-01-01T00:00:00Z",
        "2020-01-01T00:00:00Z",
        "2999-02-30T00:00:00Z",
        "2999-01-01T00:00:00+00:00",
        "2999-01-01T00:00:00.0Z",
    ] {
        let mut changed = renewed.clone();
        changed["contracts"][0]["validUntil"] = json!(expiry);
        assert!(
            verify_contract_renewal(&changed, &original, now).is_err(),
            "{expiry}"
        );
    }
    for contracts in [
        json!([]),
        json!([renewed["contracts"][0], renewed["contracts"][0]]),
        json!("bad"),
    ] {
        let mut changed = renewed.clone();
        changed["contracts"] = contracts;
        assert!(verify_contract_renewal(&changed, &original, now).is_err());
    }
    for field in ["maximumPrice", "unexpected"] {
        let mut changed = renewed.clone();
        changed["contracts"][0][field] = json!({"maximumMicros":1});
        assert!(verify_contract_renewal(&changed, &original, now).is_err());
    }
    let mut unchanged = original.clone();
    unchanged["version"] = json!("catalog-v2");
    assert!(verify_contract_renewal(&unchanged, &original, now).is_err());
    verify_contract_renewal(&renewed, &original, i64::MAX).unwrap();
}

#[test]
fn catalog_transition_classifies_non_model_hosts_without_changing_authority() {
    let original = json!({"version":"catalog-v1", "nonInferenceEgressHosts":[], "contracts":[{
        "id":"chat", "version":"v1", "validUntil":"2999-01-01T00:00:00Z",
        "providerId":"azure-openai", "endpoint":"https://models.example", "model":"model",
        "operation":"ChatCompletions", "outputField":"MaxCompletionTokens",
        "maximumInputTokens":400000, "maximumOutputTokens":128000,
        "maximumWireBytes":1048576, "outputBoundIncludesReasoning":true
    }]});
    let now = 1_800_000_000;
    let mut classified = original.clone();
    classified["nonInferenceEgressHosts"] = json!(["www.zendesk.com", "www.intercom.com"]);
    let before = classified.clone();
    verify_catalog_transition(&classified, &original, now).unwrap();
    assert_eq!(classified, before);
    verify_catalog_transition(&original, &classified, now).unwrap();
    for hosts in [
        json!(["models.example"]),
        json!(["*.example"]),
        json!(["UPPER.example"]),
        json!(["www.example:443"]),
        json!(["https://www.example"]),
        json!(null),
        json!([false]),
        json!([""]),
        json!(vec!["www.example"; 65]),
    ] {
        let mut changed = classified.clone();
        changed["nonInferenceEgressHosts"] = hosts;
        assert!(verify_catalog_transition(&changed, &original, now).is_err());
    }
    for (path, value) in [
        ("/contracts/0/model", json!("other")),
        ("/contracts/0/maximumInputTokens", json!(500000)),
        ("/contracts/0/endpoint", json!("https://other.example")),
        ("/version", json!("catalog-v2")),
    ] {
        let mut changed = classified.clone();
        *changed.pointer_mut(path).unwrap() = value;
        assert!(
            verify_catalog_transition(&changed, &original, now).is_err(),
            "{path}"
        );
    }
    let mut renewed = classified.clone();
    renewed["version"] = json!("catalog-v2");
    renewed["contracts"][0]["version"] = json!("v2");
    renewed["contracts"][0]["validUntil"] = json!("2999-02-01T00:00:00Z");
    verify_catalog_transition(&renewed, &original, now).unwrap();
    renewed["unexpected"] = json!(true);
    assert!(verify_catalog_transition(&renewed, &original, now).is_err());
}

fn additional_model_catalog() -> (Value, Value) {
    let original = json!({"version":"catalog-v1", "nonInferenceEgressHosts":[], "contracts":[{
        "id":"chat", "version":"v1", "validUntil":"2999-01-01T00:00:00Z",
        "providerId":"azure-openai", "endpoint":"https://models.example", "model":"model",
        "operation":"ChatCompletions", "outputField":"MaxCompletionTokens",
        "maximumInputTokens":400000, "maximumOutputTokens":128000,
        "maximumWireBytes":1048576, "outputBoundIncludesReasoning":true
    }]});
    let mut current = original.clone();
    current["version"] = json!("catalog-v2");
    let mut addition = original["contracts"][0].clone();
    addition["id"] = json!("full-chat");
    addition["model"] = json!("full-model");
    addition["maximumInputTokens"] = json!(1050000);
    current["contracts"]
        .as_array_mut()
        .unwrap()
        .push(addition.clone());
    addition["id"] = json!("full-responses");
    addition["operation"] = json!("Responses");
    addition["outputField"] = json!("MaxOutputTokens");
    current["contracts"].as_array_mut().unwrap().push(addition);
    (original, current)
}

#[test]
fn additive_catalog_preserves_original_authority_with_or_without_renewal() {
    for renew in [false, true] {
        let (original, mut current) = additional_model_catalog();
        if renew {
            current["contracts"][0]["version"] = json!("renewed-v2");
            current["contracts"][0]["validUntil"] = json!("2999-02-01T00:00:00Z");
        }
        let before = current.clone();
        verify_catalog_transition(&current, &original, 1_800_000_000).unwrap();
        assert_eq!(current, before);
    }
}

#[test]
fn retained_catalog_expiry_is_enforced_per_selected_route() {
    use crate::inference_budget_contract::catalog::Catalog;
    let now = 1_800_000_000;
    for expired_index in [0, 1] {
        let (mut original, mut current) = additional_model_catalog();
        original["contracts"][0]["validUntil"] = json!("2020-01-01T00:00:00Z");
        current["contracts"][0]["version"] = json!("renewed-v2");
        current["contracts"][expired_index]["validUntil"] = json!("2021-01-01T00:00:00Z");
        let before = current.clone();
        verify_catalog_transition(&current, &original, now).unwrap();
        assert_eq!(current, before);
        let catalog: Catalog = serde_json::from_value(current).unwrap();
        for (index, contract) in catalog.contracts.iter().enumerate() {
            let selected = catalog.select(
                &contract.provider_id,
                &contract.endpoint,
                &contract.model,
                contract.operation,
                now,
            );
            assert_eq!(selected.is_ok(), index != expired_index);
        }
        assert!(
            catalog
                .select(
                    "azure-openai",
                    "https://models.example",
                    "missing-model",
                    crate::inference_budget_contract::tariffs::Operation::ChatCompletions,
                    now
                )
                .is_err()
        );
    }
}

#[test]
fn additive_catalog_rejects_invalid_new_routes_and_original_authority_changes() {
    for (path, value) in [
        ("/version", json!("catalog-v1")),
        ("/contracts/0/maximumInputTokens", json!(500000)),
        ("/contracts/0/model", json!("replaced-model")),
        ("/contracts/0/endpoint", json!("https://other.example")),
        ("/contracts/1/id", json!("chat")),
        ("/contracts/1/model", json!("model")),
        ("/contracts/1/providerId", json!("")),
        ("/contracts/1/maximumInputTokens", json!(0)),
        ("/contracts/1/validUntil", json!("2000-02-30T00:00:00Z")),
        (
            "/contracts/1/validUntil",
            json!("2999-01-01T00:00:00+00:00"),
        ),
        ("/contracts/1/outputBoundIncludesReasoning", json!(false)),
        ("/contracts/1/outputField", json!("MaxOutputTokens")),
        ("/contracts/2/operation", json!("ChatCompletions")),
        ("/contracts/2/model", json!("model")),
    ] {
        let (original, mut current) = additional_model_catalog();
        *current.pointer_mut(path).unwrap() = value;
        assert!(
            verify_catalog_transition(&current, &original, 1_800_000_000).is_err(),
            "{path}"
        );
    }
    let (original, mut current) = additional_model_catalog();
    current["contracts"].as_array_mut().unwrap().remove(0);
    assert!(verify_catalog_transition(&current, &original, 1_800_000_000).is_err());
    let (original, mut current) = additional_model_catalog();
    current["unexpected"] = json!(true);
    assert!(verify_catalog_transition(&current, &original, 1_800_000_000).is_err());
}

#[test]
fn contract_renewal_requires_live_immutable_owned_adoption_of_original_intent() {
    let (mut map, reg) = fixture();
    map.metadata.name = Some("kars-sre-helm-adoption".into());
    map.data = Some(std::collections::BTreeMap::from([
        ("intentUID".into(), "intent-uid".into()),
        ("valuesDigest".into(), "a".repeat(64)),
    ]));
    verify_contract_adoption(&map, &reg).unwrap();
    for field in [
        "uid",
        "owner",
        "intent",
        "digest",
        "mutable",
        "deleted",
        "namespace",
        "name",
    ] {
        let mut changed = map.clone();
        match field {
            "uid" => changed.metadata.uid = None,
            "owner" => changed.metadata.annotations = None,
            "intent" => {
                changed
                    .data
                    .as_mut()
                    .unwrap()
                    .insert("intentUID".into(), "other".into());
            }
            "digest" => {
                changed
                    .data
                    .as_mut()
                    .unwrap()
                    .insert("valuesDigest".into(), "bad".into());
            }
            "mutable" => changed.immutable = Some(false),
            "deleted" => {
                changed.metadata.deletion_timestamp =
                    Some(serde_json::from_value(json!("2026-01-01T00:00:00Z")).unwrap())
            }
            "namespace" => changed.metadata.namespace = Some("other".into()),
            "name" => changed.metadata.name = Some("other".into()),
            _ => unreachable!(),
        }
        assert!(verify_contract_adoption(&changed, &reg).is_err(), "{field}");
    }
}

#[test]
fn renewal_requires_exact_registration_original_grant_and_immutable_intent() {
    let (_, reg) = fixture();
    let original = reg.spec.first_install_funding.as_ref().unwrap();
    let grant = crate::inference_budget_contract::ledger::TokenTopUp {
        id: "renewal".into(),
        config_uid: "config-uid".into(),
        account_uid: "account-uid".into(),
        root: original.grant.root.clone(),
        original_limits: original.grant.limits,
        tokens: 100,
        expires_at: "2999-02-01T00:00:00Z".into(),
        nodes: Default::default(),
        funding_renewal: Some(crate::inference_budget_contract::ledger::FundingRenewal {
            bridge_reactivation: None,
            registration_uid: reg.uid().unwrap(),
            intent_uid: original.intent.uid.clone(),
            intent_digest: original.intent.digest.clone(),
            original_expires_at: original.grant.expires_at.clone(),
            previous_top_up_ids: vec![],
        }),
    };
    verify_renewal_pin(&grant, &reg).unwrap();
    for field in [
        "registration",
        "intent",
        "digest",
        "expiry",
        "root",
        "limits",
    ] {
        let mut changed = grant.clone();
        let pin = changed.funding_renewal.as_mut().unwrap();
        match field {
            "registration" => pin.registration_uid = "replaced".into(),
            "intent" => pin.intent_uid = "replaced".into(),
            "digest" => pin.intent_digest = "b".repeat(64),
            "expiry" => pin.original_expires_at = "2998-01-01T00:00:00Z".into(),
            "root" => changed.root.resource.uid = "replaced".into(),
            "limits" => changed.original_limits.tokens = Some(2000),
            _ => unreachable!(),
        }
        assert!(verify_renewal_pin(&changed, &reg).is_err(), "{field}");
    }
}

#[test]
fn original_immutable_intent_resolves_without_reissuing_funding() {
    let (map, reg) = fixture();
    let original = serde_json::to_value(&reg).unwrap();
    assert_eq!(intent(&map, &reg, 100).unwrap().funding.tokens, Some(1000));
    assert_eq!(intent(&map, &reg, 101).unwrap().funding.tokens, Some(1000));
    assert_eq!(serde_json::to_value(&reg).unwrap(), original);
    assert!(intent(&map, &reg, 32_472_144_000).is_err());
}

#[test]
fn missing_replaced_changed_or_conflicting_intent_fails_closed() {
    for fault in [
        "missing",
        "uid",
        "digest",
        "mutable",
        "owner",
        "amount",
        "expiry",
        "source",
        "workspace",
        "json",
        "usd",
    ] {
        let (mut map, mut reg) = fixture();
        match fault {
            "missing" => reg.spec.first_install_funding = None,
            "uid" => map.metadata.uid = Some("replacement".into()),
            "digest" => map
                .data
                .as_mut()
                .unwrap()
                .get_mut("intent.json")
                .unwrap()
                .push(' '),
            "mutable" => map.immutable = Some(false),
            "owner" => {
                map.annotations_mut()
                    .insert("meta.helm.sh/release-name".into(), "foreign".into());
            }
            "amount" => {
                reg.spec
                    .first_install_funding
                    .as_mut()
                    .unwrap()
                    .grant
                    .limits
                    .tokens = Some(2000)
            }
            "expiry" => {
                reg.spec
                    .first_install_funding
                    .as_mut()
                    .unwrap()
                    .grant
                    .expires_at = "2998-01-01T00:00:00Z".into()
            }
            "source" => reg.spec.sandbox.uid = "replacement".into(),
            "workspace" => reg.spec.controller.namespace.uid = "replacement".into(),
            "json" => {
                map.data
                    .as_mut()
                    .unwrap()
                    .insert("intent.json".into(), "{}".into());
                reg.spec
                    .first_install_funding
                    .as_mut()
                    .unwrap()
                    .intent
                    .digest = crate::providers::signing::sha256_hex(b"{}");
            }
            "usd" => {
                reg.spec
                    .first_install_funding
                    .as_mut()
                    .unwrap()
                    .grant
                    .limits
                    .usd_micros = Some(100)
            }
            _ => unreachable!(),
        }
        assert!(intent(&map, &reg, 100).is_err(), "{fault}");
    }
}
