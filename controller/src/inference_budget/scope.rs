// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{inference_budget_contract::BudgetScope, kars_task::TaskEnvelope};

pub fn unsupported(envelope: &TaskEnvelope) -> bool {
    super::binding::has_finite(envelope)
        && envelope
            .budget
            .as_ref()
            .is_none_or(|budget| budget.scope != Some(BudgetScope::GovernedInference))
}

pub const LAUNCH_RULE: &str = "!has(self.execution) || !self.execution.launch || !has(self.envelope.budget) || ((!has(self.envelope.budget.tokens) || self.envelope.budget.tokens == 0) && (!has(self.envelope.budget.usdMicros) || self.envelope.budget.usdMicros == 0)) || (has(self.envelope.budget.scope) && self.envelope.budget.scope == 'GovernedInference')";
pub const OPT_IN_RULE: &str = "!has(self.envelope.budget) || !has(self.envelope.budget.scope) || self.envelope.budget.scope != 'GovernedInference' || ((!has(self.envelope.budget.tokens) || self.envelope.budget.tokens == 0) && (!has(self.envelope.budget.usdMicros) || self.envelope.budget.usdMicros == 0)) || (has(oldSelf.envelope.budget) && has(oldSelf.envelope.budget.scope) && oldSelf.envelope.budget.scope == 'GovernedInference' && ((has(oldSelf.envelope.budget.tokens) && oldSelf.envelope.budget.tokens > 0) || (has(oldSelf.envelope.budget.usdMicros) && oldSelf.envelope.budget.usdMicros > 0)))";
pub const RETAIN_SCOPE_RULE: &str = "!has(oldSelf.envelope.budget) || !has(oldSelf.envelope.budget.scope) || oldSelf.envelope.budget.scope != 'GovernedInference' || (has(self.envelope.budget) && has(self.envelope.budget.scope) && self.envelope.budget.scope == 'GovernedInference')";

pub fn validations()
-> Vec<k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::ValidationRule> {
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::ValidationRule;
    [
        (OPT_IN_RULE, "First finite GovernedInference opt-in requires a new Task/Team UID; an existing unbounded runtime cannot be silently converted"),
        (RETAIN_SCOPE_RULE, "GovernedInference scope cannot be removed or changed for an existing UID"),
    ].into_iter().map(|(rule, message)| ValidationRule {
        rule:rule.into(), message:Some(message.into()), reason:Some("FieldValueForbidden".into()),
        ..Default::default()
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars_task::{KarsTaskSpec, TaskBudget, TaskExecution};

    #[test]
    fn explicit_scope_is_syntax_not_permission_to_bypass_the_async_broker_gate() {
        let mut spec = KarsTaskSpec {
            envelope: TaskEnvelope {
                budget: Some(TaskBudget {
                    tokens: Some(100),
                    ..Default::default()
                }),
                ..Default::default()
            },
            execution: Some(TaskExecution {
                launch: true,
                runtime: None,
            }),
            ..Default::default()
        };
        assert!(unsupported(&spec.envelope));
        assert!(crate::kars_task::validate_execution_contract(&spec).is_err());
        spec.envelope.budget.as_mut().unwrap().scope = Some(BudgetScope::GovernedInference);
        assert!(!unsupported(&spec.envelope));
        assert!(crate::kars_task::validate_execution_contract(&spec).is_ok());
        // Actual materialization still calls binding::prepare_task and requires
        // a current, privacy-qualified broker account; the pure validator cannot
        // create that authority.
    }
}
