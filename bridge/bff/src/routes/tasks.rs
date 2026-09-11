// kars Bridge BFF — task API DTOs + handlers.
//
// These endpoints are the browser's only way to touch KarsTask resources.
// They map the typed CRD (kars::task) to stable, browser-facing JSON shapes,
// so the UI never depends on raw Kubernetes object envelopes.

mod artifacts;
mod creation;
mod diagnostics;
mod egress;
mod evidence;
mod fleet;
mod history;
mod lifecycle;
mod mapping;
mod models;
mod presentation;
mod queries;

#[cfg(test)]
mod tests;

pub use artifacts::download_artifact;
pub use creation::create_task;
pub use diagnostics::{TroubleshootDto, troubleshoot_task};
pub use egress::{
    EgressModeRequest, EgressRequest, get_learned_egress, request_egress, set_egress_mode,
};
pub use fleet::{
    AgentLifecycleDto, FleetActivityItem, FleetTelemetryDto, fleet_telemetry, list_agents,
};
pub use lifecycle::{
    HaltRequest, IncreaseTaskBudgetRequest, LaunchRequest, PromoteMissionRequest, ReplicateRequest,
    delete_task, halt_task, increase_task_budget, launch_task, promote_task, replicate_task,
};
pub use mapping::SubAgentDto;
pub use models::{
    BlueprintDto, BudgetDto, CompositionDto, CreateTaskRequest, EgressDto, EnvelopeDto,
    ExecutionDeliverableDto, ExecutionPhaseDto, ExecutionPlanDto, ExecutionRequiredToolCallDto,
    ExecutionRoleDto, ExecutionSynthesisDto, MissionArtifactDto, MissionResultDto,
    MissionTelemetryDto, ModelDto, RunBlockedDto, TaskAssignmentEventDto, TaskAssignmentStatusDto,
    TaskDetailDto, TaskSummaryDto, TeamCollaborationEventDto, TeamRolePlanDto,
};
pub use presentation::PullRequestRef;
pub(crate) use presentation::{
    classify_blocked, clean_display_name, clean_objective, deliverable_excerpt, deliverable_text,
    extract_pull_requests, is_failure_shaped_output, is_no_change_output, is_real_deliverable,
};
pub use queries::{get_task, list_tasks};

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// The sentinel a standing-team run emits when nothing changed since last time.
/// A deliverable that is ONLY this is a no-op, not a real deliverable.
pub(crate) const NO_CHANGE_SENTINEL: &str = "[[NO_MATERIAL_CHANGE]]";

/// Extracts the human intent from a leaked loop scaffold. Loop scaffolds are
/// shaped as `LOOP: <pattern>\nGOAL: <intent>\nCYCLE: …\nSUCCESS: …\nSTOP: …\n
/// SUB-AGENT INHERITANCE: …`. If a `GOAL:` line is present we return it (the
/// real intent); otherwise we drop the scaffold control lines and return what
/// remains. Plain objectives (no scaffold) pass through unchanged.
/// True when a string carries a leaked 2026 loop scaffold — used to keep the
/// control-blob out of titles, cards, URLs, and displayed objectives.
pub(crate) fn looks_scaffolded(text: &str) -> bool {
    text.starts_with("LOOP:")
        || text.contains("\nGOAL:")
        || text.starts_with("GOAL:")
        || text.contains("SUB-AGENT INHERITANCE")
        || text.contains("[[")
}

/// Map a `kube::Error` to the right client-facing error. An API rejection with
/// a 4xx status (admission/CEL/validation) is the user's invalid input — a 422
/// carrying the API server's own message — not a gateway failure.
fn map_kube_err(e: kube::Error) -> AppError {
    if let kube::Error::Api(resp) = &e
        && (400..500).contains(&resp.code)
    {
        return AppError::Rejected(resp.message.clone());
    }
    AppError::Upstream(e.to_string())
}

/// Resolve the cluster handle or surface a clear "cluster not wired" error.
pub(crate) fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}
