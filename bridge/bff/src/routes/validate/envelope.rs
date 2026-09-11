// kars Bridge BFF — envelope pre-flight checks.

use super::{Check, CheckStatus};

pub(super) async fn check_envelope(
    cluster: &crate::kars::cluster::Cluster,
    bp: &crate::routes::tasks::BlueprintDto,
    tier: Option<i32>,
    budget_tokens: Option<i64>,
    checks: &mut Vec<Check>,
) {
    // 0. Envelope sanity — the autonomy tier and budget the operator will launch
    //    with. The UI presents validation as covering the whole package, so the
    //    gate must actually check the envelope, not only the blueprint.
    if let Some(t) = tier {
        checks.push(if (1..=5).contains(&t) {
            Check {
                id: "tier".into(),
                label: format!("Autonomy tier {t}"),
                status: CheckStatus::Pass,
                detail: "Within the valid range (1–5). Sub-roles are capped one tier below.".into(),
            }
        } else {
            Check {
                id: "tier".into(),
                label: "Autonomy tier".into(),
                status: CheckStatus::Fail,
                detail: format!("Tier {t} is out of range — must be 1–5."),
            }
        });
    }
    let runtime = bp.runtime.as_deref().unwrap_or("OpenClaw");
    match budget_tokens {
        Some(b) if b <= 0 => checks.push(Check {
            id: "budget".into(),
            label: "Token budget".into(),
            status: CheckStatus::Fail,
            detail: "The budget cap must be a positive number of tokens.".into(),
        }),
        Some(b) if b < 500 => checks.push(Check {
            id: "budget".into(),
            label: format!("Token budget {b}"),
            status: CheckStatus::Warn,
            detail: "This cap is very low — a real run may stop before producing a deliverable.".into(),
        }),
        Some(b) => checks.push(Check {
            id: "budget".into(),
            label: format!("Token budget {b}"),
            status: CheckStatus::Pass,
            detail: "A per-run token cap is set — the router stops the run at this ceiling.".into(),
        }),
        None => checks.push(Check {
            id: "budget".into(),
            label: "Token budget".into(),
            status: CheckStatus::Warn,
            detail: "No token cap set — the run is bounded only by the mission's autonomy and the cluster defaults.".into(),
        }),
    }
    if let Some(plan) = bp.execution_plan.as_ref() {
        checks.push(match crate::routes::compose::validate_execution_plan(plan) {
            Ok(()) => Check {
                id: "execution_plan".into(),
                label: format!(
                    "Execution plan · {} role{} · up to {} in parallel",
                    plan.roles.len(),
                    if plan.roles.len() == 1 { "" } else { "s" },
                    plan.max_parallel
                ),
                status: CheckStatus::Pass,
                detail:
                    "Role dependencies, phases, capabilities, tool-call bounds, synthesis, and deliverables are valid."
                        .into(),
            },
            Err(error) => Check {
                id: "execution_plan".into(),
                label: "Execution plan is invalid".into(),
                status: CheckStatus::Fail,
                detail: error,
            },
        });
    }

    // 0b. Runtime image and registry credentials. This is a real launch blocker:
    // allowing a configured private image without a matching pull secret produces
    // an immediate ImagePullBackOff before the agent can execute anything.
    let runnable_runtimes = cluster.runnable_runtimes().await;
    checks.push(if runnable_runtimes.contains(runtime) {
        Check {
            id: "runtime".into(),
            label: format!("Runtime “{runtime}” can start on this cluster"),
            status: CheckStatus::Pass,
            detail: "The runtime image is configured and its registry is covered by the controller's pull credentials.".into(),
        }
    } else {
        Check {
            id: "runtime".into(),
            label: format!("Runtime “{runtime}” cannot start on this cluster"),
            status: CheckStatus::Fail,
            detail: "The runtime image is missing or its private registry is not covered by the controller's pull credentials. Launch is blocked to prevent ImagePullBackOff.".into(),
        }
    });
    if runtime != "OpenClaw" && !bp.skills.is_empty() {
        checks.push(Check {
            id: "runtime_skills".into(),
            label: format!("Runtime “{runtime}” cannot mount file skills"),
            status: CheckStatus::Fail,
            detail: "Controller-mounted file skills are currently supported only by OpenClaw; remove them or use OpenClaw instead.".into(),
        });
    }
}
