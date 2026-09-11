// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — route module aggregation.

pub mod approvals;
pub mod artifacts;
pub mod budgets;
pub mod channels;
pub mod compose;
pub mod credential_review;
pub mod digests;
pub mod efficiency;
pub mod engineering;
pub mod foundry;
pub mod github;
pub mod health;
pub mod insights;
pub mod operator;
pub mod options;
mod ownership;
pub mod receipts;
pub mod retention;
pub mod review;
pub mod run;
pub mod sre_actions;
pub mod system;
pub mod tasks;
pub mod teams;
pub mod teams_internal;
pub mod telemetry;
pub mod validate;

use axum::Router;
use axum::routing::{delete, get, post, put};

use crate::error::AppError;
use crate::state::AppState;

/// Build the application router with shared state.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/api/system", get(system::get_system))
        .route("/api/options", get(options::get_options))
        .route("/api/agents", get(tasks::list_agents))
        .route("/api/agents/fleet", get(tasks::fleet_telemetry))
        .route("/api/artifacts", get(artifacts::list_artifacts))
        .route("/api/digests", get(digests::list_digests))
        .route("/api/efficiency", get(efficiency::get_efficiency))
        .route("/api/insights", get(insights::get_insights))
        .route(
            "/api/namespaces/{ns}/tasks",
            get(tasks::list_tasks).post(tasks::create_task),
        )
        .route(
            "/api/namespaces/{ns}/teams",
            get(teams::list_teams).post(teams::create_team),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}",
            get(teams::get_team)
                .patch(teams::update_team)
                .delete(teams::delete_team),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/commons",
            get(teams::get_team_commons),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/runs/{run}/archive",
            get(teams::get_archived_run),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/promote",
            post(teams::promote_team),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/run",
            post(teams::run_team),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/runs/{run}/halt",
            post(teams::halt_team_run),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/tasks",
            get(teams::list_team_tasks).post(teams::add_team_task),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/tasks/{task_id}",
            delete(teams::delete_team_task),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/tasks/{task_id}/review",
            post(teams::review_team_task),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/engineering-source",
            get(engineering::get_source)
                .put(engineering::put_source)
                .delete(engineering::delete_source),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/engineering-source/sync",
            post(engineering::sync_now),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/engineering-review",
            post(engineering::decide_review_item),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/channels",
            get(teams::get_team_channels).post(teams::set_team_channel),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/channels/{channel}",
            delete(teams::delete_team_channel),
        )
        .route(
            "/api/namespaces/{ns}/channels",
            get(channels::get_channels).post(channels::set_channel),
        )
        .route(
            "/api/namespaces/{ns}/channels/{channel}",
            delete(channels::delete_channel),
        )
        .route(
            "/api/namespaces/{ns}/teams/{name}/ledger",
            get(teams::get_team_ledger),
        )
        .route(
            "/api/namespaces/{ns}/validate",
            post(validate::validate_package),
        )
        .route("/api/namespaces/{ns}/compose", post(compose::compose))
        .route(
            "/api/namespaces/{ns}/propose-loop",
            post(compose::propose_loop),
        )
        .route(
            "/api/namespaces/{ns}/compose-team",
            post(compose::compose_team),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}",
            get(tasks::get_task).delete(tasks::delete_task),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/artifact/{file}",
            get(tasks::download_artifact),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/receipt",
            get(receipts::get_receipt),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/receipt/verify",
            post(receipts::verify_receipt),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/compliance",
            get(receipts::compliance_pack),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/scorecard",
            get(insights::get_scorecard),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/troubleshoot",
            get(tasks::troubleshoot_task),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/launch",
            post(tasks::launch_task),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/budget",
            post(tasks::increase_task_budget),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/run",
            post(run::run_mission),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/replicate",
            post(tasks::replicate_task),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/promote",
            post(tasks::promote_task),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/halt",
            post(tasks::halt_task),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/review",
            get(review::get_review).post(review::post_review),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/validate",
            post(validate::validate_task),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/approvals",
            get(approvals::list_task_approvals),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/egress",
            post(tasks::request_egress),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/egress/learned",
            get(tasks::get_learned_egress),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/egress-mode",
            post(tasks::set_egress_mode),
        )
        .route(
            "/api/namespaces/{ns}/tasks/{name}/stream",
            get(telemetry::stream_mission),
        )
        .route(
            "/api/namespaces/{ns}/approvals",
            get(approvals::list_approvals),
        )
        .route(
            "/api/namespaces/{ns}/approvals/{name}/decision",
            post(approvals::decide_approval),
        )
        // Connect GitHub: shared App metadata + isolated per-principal connection.
        .route("/api/github/app", get(github::get_app))
        .route(
            "/api/operator/github-app",
            put(github::put_app).delete(github::delete_app),
        )
        .route(
            "/api/namespaces/{ns}/github/connection",
            get(github::get_connection).delete(github::disconnect),
        )
        .route("/api/namespaces/{ns}/github/connect", post(github::connect))
        // Operator Console surfaces (read-only projections of live CRDs).
        .route("/api/operator/sandboxes", get(operator::list_sandboxes))
        .route("/api/operator/capacity", get(operator::capacity))
        .route(
            "/api/operator/evals",
            get(operator::list_evals).post(operator::create_eval),
        )
        .route(
            "/api/operator/evals/{name}/report",
            get(operator::eval_report),
        )
        .route(
            "/api/operator/mcpservers",
            get(operator::list_mcpservers).put(operator::put_mcpserver),
        )
        .route(
            "/api/operator/mcpservers/{name}",
            delete(operator::delete_mcpserver),
        )
        .route(
            "/api/operator/mcp-profiles",
            get(operator::list_mcp_profiles).put(operator::put_mcp_profile),
        )
        .route(
            "/api/operator/mcp-profiles/{name}",
            delete(operator::delete_mcp_profile),
        )
        .route(
            "/api/operator/toolpolicies",
            get(operator::list_toolpolicies).put(operator::put_toolpolicy),
        )
        .route(
            "/api/operator/toolpolicies/{name}",
            delete(operator::delete_toolpolicy),
        )
        .route(
            "/api/operator/inferencepolicies",
            get(operator::list_inferencepolicies).put(operator::create_inferencepolicy),
        )
        .route(
            "/api/operator/inferencepolicies/{name}",
            axum::routing::patch(operator::patch_inferencepolicy)
                .delete(operator::delete_inferencepolicy),
        )
        .route("/api/operator/inference-budgets", get(budgets::get_budgets))
        .route(
            "/api/operator/inference-budgets/cluster",
            put(budgets::set_cluster_budget),
        )
        .route(
            "/api/operator/inference-budgets/workspaces/{ns}",
            put(budgets::set_workspace_budget),
        )
        .route(
            "/api/operator/inference-budgets/users/{user}",
            put(budgets::set_user_budget),
        )
        .route(
            "/api/operator/retention-policy",
            get(retention::get_retention_policy).put(retention::set_retention_policy),
        )
        .route("/api/operator/egress", get(operator::list_egress))
        .route(
            "/api/operator/egress/{name}",
            delete(operator::delete_egress),
        )
        .route("/api/operator/diagnostics", get(operator::get_diagnostics))
        .route(
            "/api/operator/orchestrator",
            get(operator::get_orchestrator),
        )
        .route(
            "/api/operator/sre-actions",
            get(sre_actions::list_sre_actions),
        )
        .route(
            "/api/operator/sre-actions/{ns}/{name}/decision",
            post(sre_actions::decide_sre_action),
        )
        .route(
            "/api/operator/integrations",
            get(operator::get_integrations),
        )
        .route(
            "/api/operator/datapath-witness",
            get(operator::datapath_witness),
        )
        .route(
            "/api/operator/skills",
            get(operator::list_skills).put(operator::put_skill),
        )
        .route(
            "/api/operator/skills/{name}",
            delete(operator::delete_skill),
        )
        .route(
            "/api/operator/skills/{name}/approve",
            post(operator::approve_skill),
        )
        .route(
            "/api/operator/skills/{name}/revoke",
            post(operator::revoke_skill),
        )
        // User-side skills: submit a package (lands PENDING) + list to see review status.
        .route(
            "/api/skills",
            get(operator::list_skills).post(operator::submit_skill),
        )
        .route(
            "/api/operator/profiles",
            get(operator::list_profiles).put(operator::put_profile),
        )
        .route(
            "/api/operator/profiles/{name}",
            delete(operator::delete_profile),
        )
        .route("/api/operator/credentials", post(operator::put_credential))
        .route(
            "/api/operator/credentials/review",
            post(credential_review::review),
        )
        .route("/api/operator/providers", post(operator::put_provider))
        .route(
            "/api/operator/providers/discover",
            post(operator::discover_models),
        )
        .route(
            "/api/operator/providers/copilot/login/start",
            post(operator::copilot_login_start),
        )
        .route(
            "/api/operator/providers/copilot/login/poll",
            post(operator::copilot_login_poll),
        )
        .route(
            "/api/operator/providers/additional",
            get(operator::list_additional_providers).put(operator::put_additional_provider),
        )
        .route(
            "/api/operator/providers/additional/{tag}",
            delete(operator::delete_additional_provider),
        )
        .route(
            "/api/operator/providers/additional/{tag}/promote",
            post(operator::promote_additional_provider),
        )
        .route(
            "/api/operator/models/default",
            post(operator::set_default_model),
        )
        .route(
            "/api/operator/local-inference/status",
            get(operator::local_inference_status),
        )
        .route(
            "/api/operator/local-inference/catalog",
            get(operator::local_inference_catalog),
        )
        .route(
            "/api/operator/local-inference/deployments",
            get(operator::list_local_model_deployments)
                .post(operator::create_local_model_deployment),
        )
        .route(
            "/api/operator/local-inference/deployments/{name}/status",
            get(operator::local_deployment_live_status),
        )
        .route(
            "/api/operator/local-inference/deployments/{name}",
            delete(operator::delete_local_model_deployment),
        )
        .route(
            "/api/operator/foundry",
            get(foundry::get_foundry)
                .post(foundry::connect_foundry)
                .delete(foundry::disconnect_foundry),
        )
        .route(
            "/api/operator/foundry/verify",
            post(foundry::verify_foundry),
        )
        .route("/api/operator/audit", get(operator::get_audit))
        // Internal Teams gateway decision endpoint (not browser-facing).
        .route(
            "/api/internal/teams/decision",
            post(teams_internal::teams_decision),
        )
        .route(
            "/api/internal/teams/command",
            post(teams_internal::teams_command),
        )
        .fallback(not_found)
        .with_state(state)
}

/// Structured 404 for any unmatched route, using the shared error envelope.
async fn not_found() -> AppError {
    AppError::NotFound
}
