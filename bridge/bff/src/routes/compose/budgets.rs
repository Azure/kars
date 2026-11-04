// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::kars::cluster::Cluster;
use crate::routes::validate::budget_contracts::{coverage, parse_catalog, read_catalog};

use super::{ComposeProposal, ComposeTeamProposal};

pub(super) fn validate_cap(cap: Option<i64>) -> Result<(), String> {
    if cap.is_some_and(|cap| !(1..=9_007_199_254_740_991).contains(&cap)) {
        return Err("Token caps must be positive safe whole numbers when specified.".into());
    }
    Ok(())
}

/// Check raw values before lossy proposal normalization. Only null/absence means
/// no token ceiling; malformed limits must invalidate the reviewed plan.
pub(super) fn validate_proposed_caps(json: &serde_json::Value) -> Result<(), String> {
    let check = |value: Option<&serde_json::Value>, path: &str| {
        let cap = match value {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => Some(value.as_i64().ok_or_else(|| {
                format!("{path} must be a positive safe integer or null; malformed caps cannot be discarded")
            })?),
        };
        validate_cap(cap).map_err(|error| format!("{path}: {error}"))
    };
    check(json.get("budget_tokens"), "budget_tokens")?;
    if let Some(roles) = json
        .pointer("/execution_plan/roles")
        .and_then(|v| v.as_array())
    {
        for (index, role) in roles.iter().enumerate() {
            check(
                role.get("budget_tokens"),
                &format!("execution_plan.roles[{index}].budget_tokens"),
            )?;
        }
    }
    Ok(())
}

pub(super) struct BudgetContext {
    raw: Result<String, String>,
}

impl BudgetContext {
    pub(super) async fn load(cluster: &Cluster) -> Self {
        Self {
            raw: read_catalog(cluster).await,
        }
    }

    pub(super) fn prompt(&self, models: &[crate::routes::options::ModelOption]) -> String {
        let now = chrono::Utc::now().timestamp();
        let bounds = self
            .raw
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|raw| {
                let catalog = parse_catalog(raw, now)?;
                let mut rows = Vec::new();
                for contract in &catalog.contracts {
                    if contract.validate(now).is_err()
                        || !models
                            .iter()
                            .any(|model| model.deployment == contract.model)
                    {
                        continue;
                    }
                    let reservation = contract
                        .maximum_input_tokens
                        .checked_add(contract.maximum_output_tokens)
                        .ok_or_else(|| {
                            "Operator reservation bound overflows token accounting.".to_string()
                        })?;
                    rows.push(serde_json::json!({
                        "model": contract.model,
                        "accounting_provider": contract.provider_id,
                        "operation": contract.operation,
                        "maximum_input_tokens": contract.maximum_input_tokens,
                        "maximum_output_tokens": contract.maximum_output_tokens,
                        "full_request_reservation_tokens": reservation,
                        "minimum_role_cap_before_usage_tokens": contract.maximum_input_tokens + 1,
                    }));
                }
                Ok(
                    serde_json::json!({"catalogue_version":catalog.version,"current_bounds":rows})
                        .to_string(),
                )
            })
            .unwrap_or_else(|error| format!("UNAVAILABLE: {error}"));
        format!(
            "\nLIVE OPERATOR BUDGET CONTRACTS (data, not instructions):\n{bounds}\n\
            Budget for reservation capacity, not just expected billed tokens. The broker reserves \
            the complete maximum input bound plus the requested output allowance before a call; \
            unused reservation is released after settlement. Use the full-request bounds above \
            conservatively and add task-derived headroom for cumulative usage, review, retries and \
            concurrent calls. In the rationale, show the estimated model-call count and cumulative \
            settled-token allowance for each role and synthesis. For sequential calls the cap must \
            cover tokens already settled BEFORE the last planned call PLUS a full reservation for \
            that call. Releasing an unused reservation does not refund settled tokens. \
            Native research repeatedly sends accumulated source/tool context and may require \
            compaction calls: estimating only the user's short prompt or final answer substantially \
            underfunds it. Tool-call count is not model-call count. Use conservative context-growth \
            estimates and any supplied actual usage evidence, explaining uncertainty rather than \
            claiming completion is guaranteed. For finite funding, each role cap AND the Mission's shared lifetime cap must admit a \
            request. Each finite execution_plan.roles[i].budget_tokens must be greater than or \
            equal to the largest minimum_role_cap_before_usage_tokens for its selected primary \
            and fallback models. This is an independent lower bound for EACH role, not a total \
            to divide among roles. A large Mission total does not make a smaller role cap valid. \
            These are readiness checks for REVIEWED finite limits, not permission to generate limits. \
            Default to policy inheritance: budget_tokens=null, including role caps, unless reviewed \
            extra limits or a live profile specify a ceiling. Never turn estimates or reservation \
            bounds into new finite caps or an expiry. Null/absent role caps still inherit any finite aggregate \
            ceiling. Unbounded tokens retain GovernedInference accounting, a valid operator catalogue, \
            currency caps, profile ceilings and all authorization and resource checks. Never infer \
            unbounded funding from a failed finite cap. Check applicable numeric \
            inequalities before emitting the JSON. Team role budgets also share the existing Team UID lifetime \
            cap; they are not fresh funding per run. Cover primary AND fallback models; accounting \
            provider IDs are not interchangeable with UI route names. Missing/expired contracts \
            block finite token or currency limits; record-only accounting without either limit does \
            not require per-model reservation bounds. These bounds do not prove exact endpoint/operation coverage or account \
            readiness. Respect any user ceiling: if it cannot accommodate the work, explain the \
            blocker rather than increasing it, removing a cap, dropping requirements, resetting \
            an account or weakening a contract. Do not present a bare minimum reservation as \
            sufficient funding for a complete task.\n"
        )
    }

    fn check(&self, models: &[String], tokens: Option<i64>) -> Result<(), String> {
        self.check_limits(models, tokens, false)
    }

    fn check_limits(
        &self,
        models: &[String],
        tokens: Option<i64>,
        money_required: bool,
    ) -> Result<(), String> {
        validate_cap(tokens)?;
        coverage(
            self.raw.as_ref().map_err(Clone::clone)?,
            models,
            money_required,
            chrono::Utc::now().timestamp(),
            tokens,
        )
    }

    pub(super) fn mission_review_error(
        &self,
        proposal: &ComposeProposal,
        raw: &str,
    ) -> Option<String> {
        review_errors(
            super::mission::mission_proposal_structure_error(proposal, raw),
            self.mission(proposal),
        )
    }

    pub(super) fn team_review_error(
        &self,
        proposal: &ComposeTeamProposal,
        execution_plan_error: Option<&str>,
    ) -> Option<String> {
        review_errors(
            (!super::team::team_proposal_is_complete(proposal)).then(|| {
                super::team::incomplete_team_proposal_detail(proposal, execution_plan_error)
            }),
            self.team(proposal),
        )
    }

    pub(super) fn mission(&self, proposal: &ComposeProposal) -> Result<(), String> {
        let model = proposal
            .model
            .as_ref()
            .ok_or("Missing model route for budget review")?;
        let models = std::iter::once(model.deployment.clone())
            .chain(
                proposal
                    .model_fallbacks
                    .iter()
                    .map(|model| model.deployment.clone()),
            )
            .collect::<Vec<_>>();
        let mut errors = self
            .check(&models, proposal.budget_tokens)
            .err()
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(plan) = &proposal.execution_plan {
            for role in &plan.roles {
                if let Err(error) = validate_cap(role.budget_tokens).and_then(|()| {
                    self.check(
                        &models,
                        plan.role_qualification_budget(&role.name, proposal.budget_tokens),
                    )
                }) {
                    errors.push(format!("Role {:?}: {error}", role.name));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    pub(super) fn team(&self, proposal: &ComposeTeamProposal) -> Result<(), String> {
        let Some(plan) = &proposal.execution_plan else {
            return Ok(());
        };
        let mut errors = Vec::new();
        let models = std::iter::once(proposal.model.as_str())
            .chain(proposal.model_fallbacks.iter().map(String::as_str))
            .map(|route| {
                route
                    .split_once("::")
                    .map_or(route, |(_, model)| model)
                    .to_string()
            })
            .collect::<Vec<_>>();
        let money_required = proposal.profile.as_ref().is_some_and(|profile| {
            profile
                .spec
                .pointer("/defaultEnvelope/budget/usdMicros")
                .is_some_and(|value| !value.is_null())
        });
        if let Err(error) = self.check_limits(&models, proposal.budget_tokens, money_required) {
            errors.push(format!("Team lifetime budget: {error}"));
        }
        for role in &plan.roles {
            let route = proposal
                .roles
                .iter()
                .find(|member| member.name == role.name)
                .map(|member| member.model.as_str())
                .filter(|model| !model.is_empty())
                .unwrap_or(&proposal.model);
            let models = std::iter::once(route)
                .chain(proposal.model_fallbacks.iter().map(String::as_str))
                .map(|route| {
                    route
                        .split_once("::")
                        .map_or(route, |(_, model)| model)
                        .to_string()
                })
                .collect::<Vec<_>>();
            if let Err(error) = validate_cap(role.budget_tokens).and_then(|()| {
                self.check_limits(
                    &models,
                    plan.role_qualification_budget(&role.name, proposal.budget_tokens),
                    money_required,
                )
            }) {
                errors.push(format!("Role {:?}: {error}", role.name));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }
}

fn review_errors(structure: Option<String>, budget: Result<(), String>) -> Option<String> {
    let errors = structure
        .into_iter()
        .chain(budget.err())
        .collect::<Vec<_>>();
    (!errors.is_empty()).then(|| errors.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context() -> BudgetContext {
        BudgetContext {
            raw: Ok(json!({"version":"test-v1","contracts":[{
                "id":"chat","version":"v1","validUntil":"2030-01-01T00:00:00Z",
                "providerId":"azure-openai","endpoint":"https://models.example",
                "model":"gpt-5.4-mini","operation":"ChatCompletions","outputField":"MaxTokens",
                "maximumInputTokens":400000,"maximumOutputTokens":8192,"maximumWireBytes":1048576,
                "outputBoundIncludesReasoning":true
            }]})
            .to_string()),
        }
    }

    fn proposal_fixture() -> (crate::routes::options::Options, serde_json::Value) {
        let (mut options, mut raw) = super::super::capability_tests::team_role_binding_fixture();
        options
            .models
            .retain(|model| model.deployment == "gpt-5.4-mini");
        raw["model"] = json!("azure-foundry::gpt-5.4-mini");
        for role in raw["roles"].as_array_mut().unwrap() {
            role["model"] = json!("azure-foundry::gpt-5.4-mini");
        }
        (options, raw)
    }

    #[test]
    fn unbounded_composition_keeps_contract_checks_and_mixed_finite_limits() {
        use super::super::capability_tests::test_efficiency;
        let (options, mut raw) = proposal_fixture();
        let context = context();
        for (aggregate, role_cap, valid) in [
            (None, None, true),
            (Some(600000), None, true),
            (None, Some(600000), true),
            (Some(600000), Some(600000), true),
            (Some(12000), None, false),
            (None, Some(12000), false),
            (Some(12000), Some(600000), false),
            (Some(600000), Some(12000), false),
        ] {
            raw["budget_tokens"] = json!(aggregate);
            for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
                role["budget_tokens"] = json!(role_cap);
            }
            let (team, _, _) = super::super::team_proposal::parse_and_validate_team(
                &raw.to_string(),
                &options,
                &test_efficiency(),
            );
            let before = serde_json::to_value(&team).unwrap();
            assert_eq!(context.team_review_error(&team, None).is_none(), valid);
            assert_eq!(serde_json::to_value(&team).unwrap(), before);
            let mut mission_raw = raw.clone();
            mission_raw["runtime"] = json!("Hermes");
            mission_raw["model"] = json!({"provider":"azure-foundry","deployment":"gpt-5.4-mini"});
            let (mission, _) = super::super::mission_proposal::parse_and_validate(
                &mission_raw.to_string(),
                &options,
                &test_efficiency(),
            );
            let before = serde_json::to_value(&mission).unwrap();
            assert_eq!(
                context
                    .mission_review_error(&mission, &mission_raw.to_string())
                    .is_none(),
                valid
            );
            assert_eq!(serde_json::to_value(&mission).unwrap(), before);
            assert_eq!(mission.budget_tokens, aggregate);
            assert_eq!(team.budget_tokens, aggregate);
            if aggregate.is_none() && role_cap.is_none() {
                for (broken, record_only_allowed) in [
                    (
                        BudgetContext {
                            raw: Err("catalogue unavailable".into()),
                        },
                        false,
                    ),
                    (
                        BudgetContext {
                            raw: Ok("invalid json".into()),
                        },
                        false,
                    ),
                    (
                        BudgetContext {
                            raw: Ok(context
                                .raw
                                .as_ref()
                                .unwrap()
                                .replace("2030-01-01", "2000-01-01")),
                        },
                        true,
                    ),
                    (
                        BudgetContext {
                            raw: Ok(context
                                .raw
                                .as_ref()
                                .unwrap()
                                .replace("gpt-5.4-mini", "another-model")),
                        },
                        true,
                    ),
                ] {
                    assert_eq!(broken.team(&team).is_ok(), record_only_allowed);
                    assert_eq!(broken.mission(&mission).is_ok(), record_only_allowed);
                    let mut finite_team = team.clone();
                    finite_team.budget_tokens = Some(600000);
                    let mut finite_mission = mission.clone();
                    finite_mission.budget_tokens = Some(600000);
                    assert!(broken.team(&finite_team).is_err());
                    assert!(broken.mission(&finite_mission).is_err());
                }
                let mut incomplete = team.clone();
                incomplete.roles.pop();
                assert!(context.team_review_error(&incomplete, None).is_some());
                incomplete = team.clone();
                incomplete.execution_plan = None;
                assert!(context.team_review_error(&incomplete, None).is_some());
                let mut invalid_resources = raw.clone();
                invalid_resources["roles"][0]["resources"] = json!({"mcp_servers":17});
                let (invalid_team, _, _) = super::super::team_proposal::parse_and_validate_team(
                    &invalid_resources.to_string(),
                    &options,
                    &test_efficiency(),
                );
                assert!(context.team_review_error(&invalid_team, None).is_some());
                let mut invalid_model = mission_raw.clone();
                invalid_model["model"] = json!({"provider":"unknown","deployment":"missing"});
                let (invalid_mission, _) = super::super::mission_proposal::parse_and_validate(
                    &invalid_model.to_string(),
                    &options,
                    &test_efficiency(),
                );
                assert!(
                    context
                        .mission_review_error(&invalid_mission, &invalid_model.to_string())
                        .is_some()
                );
                let mut single = mission.clone();
                single.execution_plan.as_mut().unwrap().roles.clear();
                assert!(context.mission(&single).is_ok());
                assert!(
                    BudgetContext {
                        raw: Err("catalogue unavailable".into())
                    }
                    .mission(&single)
                    .is_err()
                );
                single.model = None;
                assert!(context.mission(&single).is_err());
                let mut fallback = mission.clone();
                fallback.model_fallbacks.push(super::super::ComposeModel {
                    provider: "azure-foundry".into(),
                    deployment: "unknown-model".into(),
                });
                assert!(context.mission(&fallback).is_ok());
                fallback.budget_tokens = Some(600000);
                assert!(context.mission(&fallback).is_err());
                let mut fallback = team.clone();
                fallback
                    .model_fallbacks
                    .push("azure-foundry::unknown-model".into());
                assert!(context.team(&fallback).is_ok());
                fallback.budget_tokens = Some(600000);
                assert!(context.team(&fallback).is_err());
                fallback.model_fallbacks.clear();
                fallback.roles[0].model = "azure-foundry::unknown-model".into();
                assert!(context.team(&fallback).is_err());
            }
        }
    }

    #[test]
    fn unbounded_team_tokens_keep_profile_currency_contracts_on_every_route() {
        let (options, mut raw) = proposal_fixture();
        raw["budget_tokens"] = json!(null);
        let (mut team, _, _) = super::super::team_proposal::parse_and_validate_team(
            &raw.to_string(),
            &options,
            &super::super::capability_tests::test_efficiency(),
        );
        team.profile = Some(crate::routes::operator::ProfileDto {
            name: "currency-ceiling".into(),
            namespace: "workspace".into(),
            domain: None,
            phase: Some("Ready".into()),
            template_digest: None,
            display_name: None,
            charter_template: None,
            tier: Some(3),
            tool_policy: None,
            knowledge_commons: None,
            roles: vec![],
            spec: json!({"defaultEnvelope":{"tier":3,"authorityCeiling":3,
                "budget":{"scope":"GovernedInference","usdMicros":500000}}}),
        });
        let mut context = context();
        let before = serde_json::to_value(&team).unwrap();
        let error = context.team(&team).unwrap_err();
        assert!(error.contains("Team lifetime budget"));
        assert!(error.contains("maximum prices"));
        for role in &team.roles {
            assert!(error.contains(&format!("Role {:?}", role.name)));
        }
        let mut catalog: serde_json::Value =
            serde_json::from_str(context.raw.as_ref().unwrap()).unwrap();
        catalog["contracts"][0]["maximumPrice"] = json!({"kind":"perRequest","maximumMicros":1000});
        context.raw = Ok(catalog.to_string());
        assert!(context.team(&team).is_ok());
        assert_eq!(serde_json::to_value(&team).unwrap(), before);
        let mut unpriced = catalog["contracts"][0].clone();
        unpriced["id"] = json!("unpriced");
        unpriced["model"] = json!("unpriced-model");
        unpriced.as_object_mut().unwrap().remove("maximumPrice");
        catalog["contracts"].as_array_mut().unwrap().push(unpriced);
        context.raw = Ok(catalog.to_string());
        team.roles[0].model = "azure-foundry::unpriced-model".into();
        assert!(context.team(&team).unwrap_err().contains("maximum prices"));
        team.roles[0].model = "azure-foundry::gpt-5.4-mini".into();
        team.model_fallbacks
            .push("azure-foundry::unpriced-model".into());
        assert!(context.team(&team).unwrap_err().contains("maximum prices"));
    }

    #[test]
    fn malformed_raw_caps_cannot_normalize_into_an_unbounded_reviewable_proposal() {
        use super::super::capability_tests::test_efficiency;
        let (options, raw) = proposal_fixture();
        for path in ["/budget_tokens", "/execution_plan/roles/0/budget_tokens"] {
            for invalid in [
                json!(0),
                json!(-1),
                json!(9007199254740992_i64),
                json!(i64::MAX),
                json!(u64::MAX),
                json!(1.5),
                json!("unlimited"),
                json!("600000"),
                json!(true),
                json!({}),
                json!([]),
            ] {
                let mut raw = raw.clone();
                raw["budget_tokens"] = json!(null);
                raw["execution_plan"]["roles"][0]["budget_tokens"] = json!(null);
                *raw.pointer_mut(path).unwrap() = invalid.clone();
                let error =
                    super::super::execution::execution_plan_error_from_raw(&raw.to_string())
                        .unwrap();
                assert!(
                    error.contains("budget_tokens"),
                    "{path}: {invalid}: {error}"
                );
                let (team, _, _) = super::super::team_proposal::parse_and_validate_team(
                    &raw.to_string(),
                    &options,
                    &test_efficiency(),
                );
                assert!(team.execution_plan.is_none(), "{path}: {invalid}");
                assert!(context().team_review_error(&team, Some(&error)).is_some());
                raw["runtime"] = json!("Hermes");
                raw["model"] = json!({"provider":"azure-foundry","deployment":"gpt-5.4-mini"});
                let (mission, _) = super::super::mission_proposal::parse_and_validate(
                    &raw.to_string(),
                    &options,
                    &test_efficiency(),
                );
                assert!(mission.execution_plan.is_none());
                assert!(
                    context()
                        .mission_review_error(&mission, &raw.to_string())
                        .is_some()
                );
            }
        }
        for cap in [
            None,
            Some(json!(null)),
            Some(json!(600000)),
            Some(json!(9007199254740991_i64)),
        ] {
            let mut raw = raw.clone();
            raw.as_object_mut().unwrap().remove("budget_tokens");
            for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
                role.as_object_mut().unwrap().remove("budget_tokens");
                if let Some(cap) = &cap {
                    role["budget_tokens"] = cap.clone();
                }
            }
            if let Some(cap) = &cap {
                raw["budget_tokens"] = cap.clone();
            }
            assert!(validate_proposed_caps(&raw).is_ok());
            assert!(super::super::execution::parse_execution_plan(&raw).is_some());
        }
    }

    #[test]
    fn composition_uses_the_same_live_minimum_as_preflight() {
        let context = context();
        let models = vec!["gpt-5.4-mini".to_string()];
        assert!(
            context
                .check(&models, Some(12000))
                .unwrap_err()
                .contains("400001")
        );
        assert!(context.check(&models, Some(500000)).is_ok());
        assert!(context.check(&models, Some(0)).is_err());
        assert!(
            context
                .check(&["unknown-model".into()], Some(500000))
                .is_err()
        );
    }

    #[test]
    fn prompt_includes_current_selected_model_bounds_and_excludes_expired_entries() {
        let model = crate::routes::options::ModelOption {
            provider: "azure-foundry".into(),
            deployment: "gpt-5.4-mini".into(),
            is_default: true,
            detail: None,
        };
        let mut context = context();
        let prompt = context.prompt(std::slice::from_ref(&model));
        assert!(prompt.contains("\"full_request_reservation_tokens\":408192"));
        assert!(prompt.contains("\"maximum_input_tokens\":400000"));
        assert!(prompt.contains("\"minimum_role_cap_before_usage_tokens\":400001"));
        assert!(prompt.contains("independent lower bound for EACH role"));
        assert!(prompt.contains("not just expected billed tokens"));
        assert!(prompt.contains("BEFORE the last planned call PLUS a full reservation"));
        assert!(prompt.contains("Tool-call count is not model-call count"));
        assert!(prompt.contains("compaction calls"));
        assert!(prompt.contains("any supplied actual usage evidence"));
        assert!(prompt.contains("existing Team UID lifetime"));
        assert!(prompt.contains("Default to policy inheritance: budget_tokens=null"));
        assert!(prompt.contains("GovernedInference accounting"));
        assert!(prompt.contains("currency caps, profile ceilings"));
        let mut catalog: serde_json::Value =
            serde_json::from_str(context.raw.as_ref().unwrap()).unwrap();
        catalog["contracts"][0]["maximumInputTokens"] = json!(6000);
        context.raw = Ok(catalog.to_string());
        let prompt = context.prompt(std::slice::from_ref(&model));
        assert!(prompt.contains("\"minimum_role_cap_before_usage_tokens\":6001"));
        assert!(prompt.contains("\"full_request_reservation_tokens\":14192"));
        assert!(!prompt.contains("400001"));
        assert!(
            context
                .check(&[model.deployment.clone()], Some(6000))
                .is_err()
        );
        assert!(
            context
                .check(&[model.deployment.clone()], Some(6001))
                .is_ok()
        );
        catalog["contracts"][0]["validUntil"] = json!("2000-01-01T00:00:00Z");
        context.raw = Ok(catalog.to_string());
        assert!(!context.prompt(&[model]).contains("408192"));
        assert!(
            context
                .check(&["gpt-5.4-mini".into()], Some(500000))
                .is_err()
        );
    }

    #[test]
    fn mission_and_team_role_caps_are_checked_without_rewriting_the_proposal() {
        use super::super::capability_tests::{team_role_binding_fixture, test_efficiency};
        let (mut options, mut raw) = team_role_binding_fixture();
        options.models = vec![crate::routes::options::ModelOption {
            provider: "azure-foundry".into(),
            deployment: "gpt-5.4-mini".into(),
            is_default: true,
            detail: None,
        }];
        raw["model"] = json!("azure-foundry::gpt-5.4-mini");
        for role in raw["roles"].as_array_mut().unwrap() {
            role["model"] = json!("azure-foundry::gpt-5.4-mini");
        }
        for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
            role["budget_tokens"] = json!(600000);
        }
        let (mut team, _, _) = super::super::team_proposal::parse_and_validate_team(
            &raw.to_string(),
            &options,
            &test_efficiency(),
        );
        let context = context();
        assert_eq!(team.budget_tokens, Some(3000000));
        assert!(context.team(&team).is_ok());
        for cap in [Some(0), Some(-1), Some(9_007_199_254_740_992)] {
            team.budget_tokens = cap;
            let before = serde_json::to_value(&team).unwrap();
            assert!(
                context
                    .team(&team)
                    .unwrap_err()
                    .contains("positive safe whole numbers")
            );
            assert_eq!(serde_json::to_value(&team).unwrap(), before);
        }
        team.budget_tokens = Some(12000);
        let error = context.team(&team).unwrap_err();
        assert!(error.contains("Team lifetime budget"), "{error}");
        assert!(error.contains("400001"), "{error}");
        assert!(error.contains("Role"), "{error}");
        team.budget_tokens = Some(3000000);
        team.execution_plan.as_mut().unwrap().roles[0].budget_tokens = Some(12000);
        let before = serde_json::to_value(&team).unwrap();
        assert!(context.team(&team).unwrap_err().contains("400001"));
        assert_eq!(serde_json::to_value(&team).unwrap(), before);
        let mut mission = ComposeProposal {
            tier: 3,
            model: Some(super::super::ComposeModel {
                provider: "azure-foundry".into(),
                deployment: "gpt-5.4-mini".into(),
            }),
            model_fallbacks: vec![],
            model_basis: None,
            runtime: "Hermes".into(),
            instructions: team.instructions.clone(),
            tool_policy: None,
            mcp_servers: vec![],
            skills: vec![],
            egress: vec![],
            isolation: "standard".into(),
            memory: None,
            budget_tokens: Some(3000000),
            execution_plan: team.execution_plan.clone(),
            delegation: super::super::execution::single_agent_delegation(),
        };
        let before = serde_json::to_value(&mission).unwrap();
        assert!(context.mission(&mission).unwrap_err().contains("Role"));
        assert_eq!(serde_json::to_value(&mission).unwrap(), before);
        mission.execution_plan.as_mut().unwrap().roles[0].budget_tokens = Some(600000);
        assert!(context.mission(&mission).is_ok());
        mission.budget_tokens = Some(12000);
        assert!(context.mission(&mission).unwrap_err().contains("400001"));
        mission.budget_tokens = Some(3000000);
        mission.model_fallbacks.push(super::super::ComposeModel {
            provider: "azure-foundry".into(),
            deployment: "uncontracted-model".into(),
        });
        assert!(context.mission(&mission).is_err());
    }

    #[test]
    fn single_repair_reports_egress_and_all_role_budget_errors_without_mutation() {
        use super::super::capability_tests::{team_role_binding_fixture, test_efficiency};
        let (mut options, mut raw) = team_role_binding_fixture();
        options.models = vec![crate::routes::options::ModelOption {
            provider: "azure-foundry".into(),
            deployment: "gpt-5.4-mini".into(),
            is_default: true,
            detail: None,
        }];
        raw["egress"] = json!([]);
        raw["budget_tokens"] = json!(3000000);
        raw["model"] = json!("azure-foundry::gpt-5.4-mini");
        for role in raw["roles"].as_array_mut().unwrap() {
            role["model"] = json!("azure-foundry::gpt-5.4-mini");
        }
        for role in raw["execution_plan"]["roles"].as_array_mut().unwrap() {
            role["budget_tokens"] = json!(220000);
            for phase in role["phases"].as_array_mut().unwrap() {
                phase["fresh_context"] = json!(true);
                phase["capabilities"] = json!(["filesystem-read", "filesystem-write"]);
                phase["max_tool_calls"] = json!(8);
            }
        }
        raw["execution_plan"]["synthesis"]["capabilities"] =
            json!(["filesystem-read", "filesystem-write"]);
        raw["execution_plan"]["synthesis"]["max_tool_calls"] = json!(8);
        let phase = &mut raw["execution_plan"]["roles"][0]["phases"][0];
        phase["capabilities"] = json!(["filesystem-read", "filesystem-write", "network"]);
        phase["required_tool_calls"] = json!([{
            "name":"http_fetch", "arguments":{
                "url":"https://support.freshdesk.com/support/solutions", "method":"GET"
            }
        }]);
        phase["min_tool_calls"] = json!(1);
        let (team, _, _) = super::super::team_proposal::parse_and_validate_team(
            &raw.to_string(),
            &options,
            &test_efficiency(),
        );
        let before = serde_json::to_value(&team).unwrap();
        let context = context();
        let error = context.team_review_error(&team, None).unwrap();
        assert!(error.contains("support.freshdesk.com"), "{error}");
        assert!(error.contains("400001"), "{error}");
        for role in &team.execution_plan.as_ref().unwrap().roles {
            assert!(error.contains(&format!("Role {:?}:", role.name)), "{error}");
        }
        assert_eq!(serde_json::to_value(&team).unwrap(), before);
        let mut team = team;
        team.egress.push(super::super::ComposeEgress {
            host: "freshdesk.com".into(),
            port: Some(443),
        });
        let error = context.team_review_error(&team, None).unwrap();
        assert!(error.contains("400001"), "{error}");
        assert!(!error.contains("support.freshdesk.com"), "{error}");
        for role in &mut team.execution_plan.as_mut().unwrap().roles {
            role.budget_tokens = Some(600000);
        }
        assert!(context.team_review_error(&team, None).is_none());
        team.egress.clear();
        let error = context.team_review_error(&team, None).unwrap();
        assert!(error.contains("support.freshdesk.com"), "{error}");
        assert!(!error.contains("400001"), "{error}");
        let unavailable = BudgetContext {
            raw: Err("catalogue unavailable".into()),
        };
        assert!(
            unavailable
                .team_review_error(&team, None)
                .unwrap()
                .contains("catalogue unavailable")
        );

        raw["runtime"] = json!("Hermes");
        raw["model"] = json!({"provider":"azure-foundry","deployment":"gpt-5.4-mini"});
        let (mut mission, _) = super::super::mission_proposal::parse_and_validate(
            &raw.to_string(),
            &options,
            &test_efficiency(),
        );
        let before = serde_json::to_value(&mission).unwrap();
        let error = context
            .mission_review_error(&mission, &raw.to_string())
            .unwrap();
        assert!(error.contains("support.freshdesk.com"), "{error}");
        assert!(error.contains("400001"), "{error}");
        for role in &mission.execution_plan.as_ref().unwrap().roles {
            assert!(error.contains(&format!("Role {:?}:", role.name)), "{error}");
        }
        assert_eq!(serde_json::to_value(&mission).unwrap(), before);
        for role in &mut mission.execution_plan.as_mut().unwrap().roles {
            role.budget_tokens = Some(600000);
        }
        mission.egress.push(super::super::ComposeEgress {
            host: "freshdesk.com".into(),
            port: Some(443),
        });
        assert!(
            context
                .mission_review_error(&mission, &raw.to_string())
                .is_none()
        );
        mission
            .execution_plan
            .as_mut()
            .unwrap()
            .synthesis
            .max_tool_calls = 1;
        let review = context.mission_review_error(&mission, &raw.to_string());
        assert!(review.is_none(), "An activation blocker remains reviewable");
        let feedback =
            super::super::execution::repair_feedback(review, mission.execution_plan.as_ref())
                .unwrap();
        assert!(feedback.contains("synthesis needs at least"), "{feedback}");
        mission.budget_tokens = None;
        for role in &mut mission.execution_plan.as_mut().unwrap().roles {
            role.budget_tokens = Some(220000);
        }
        let before = serde_json::to_value(&mission).unwrap();
        let review = context.mission_review_error(&mission, &raw.to_string());
        let feedback =
            super::super::execution::repair_feedback(review, mission.execution_plan.as_ref())
                .unwrap();
        for expected in ["400001", "synthesis needs at least"] {
            assert!(feedback.contains(expected), "{feedback}");
        }
        assert_eq!(serde_json::to_value(&mission).unwrap(), before);
        team.execution_plan
            .as_mut()
            .unwrap()
            .synthesis
            .max_tool_calls = 1;
        let review = context.team_review_error(&team, None);
        let feedback =
            super::super::execution::repair_feedback(review, team.execution_plan.as_ref()).unwrap();
        assert!(feedback.contains("synthesis needs at least"), "{feedback}");
        assert!(feedback.contains("support.freshdesk.com"), "{feedback}");
        assert!(super::super::execution::repair_feedback(None, None).is_none());
    }

    #[test]
    fn prompt_reports_unavailable_catalogue_without_inventing_bounds() {
        let context = BudgetContext {
            raw: Err("catalogue absent".into()),
        };
        let prompt = context.prompt(&[]);
        assert!(prompt.contains("UNAVAILABLE: catalogue absent"));
        assert!(!prompt.contains("400000"));
        assert!(prompt.contains("Respect any user ceiling"));
        assert!(context.check(&["model".into()], Some(500000)).is_err());
    }
}
