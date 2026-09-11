// kars Bridge BFF — the launch-package orchestrator (§20 "intent → package").
//
// Turns a plain-language objective into a *proposed*, fully-governed launch
// package — the trust envelope (autonomy tier, budget), the model, harness,
// standing instructions, tool policy, connected MCP servers, network egress,
// isolation, and shared memory — composed by an LLM that is constrained to the
// REAL building blocks this cluster offers (from `/api/options`).
//
// Honesty + safety:
// - This is a PROPOSAL, not an action. Nothing is provisioned. The operator
//   reviews and edits every field, then the §20 pre-flight validation gate and
//   the explicit Launch step still govern what actually runs.
// - The LLM may only reference building blocks that exist; the server
//   re-validates the proposal against the live options and drops/normalizes
//   anything that doesn't, so the orchestrator can never invent a model, tool
//   policy, MCP server, or isolation level the cluster can't honor.
// - The orchestrator endpoint + credentials are BFF-side config. When they are
//   not set the endpoint reports `available: false` and the UI falls back to the
//   manual composer — never a fabricated package.

mod client;
mod egress;
mod execution;
mod loops;
mod mission;
mod mission_proposal;
mod models;
mod prompts;
mod routing;
mod team;
mod team_proposal;
mod team_qualification;

#[cfg(test)]
mod capability_tests;

pub(crate) use execution::{
    delegation_budget_allocation, validate_delegation, validate_execution_plan,
};
pub use loops::{ProposeLoopRequest, ProposeLoopResponse, propose_loop};
pub use mission::compose;
pub use models::{
    ComposeDelegation, ComposeDelegationRole, ComposeEgress, ComposeModel, ComposeProposal,
    ComposeRequest, ComposeResponse,
};
pub use team::{
    ComposeTeamMilestone, ComposeTeamProposal, ComposeTeamRequest, ComposeTeamResponse,
    ComposeTeamRole, compose_team,
};

/// The cluster-default governance policy. Assigned to any envelope the
/// orchestrator (or operator) leaves un-governed, so the sandbox is BOTH
/// governed and functional: the agent runtime always initializes its AGT
/// engine and fails closed on an empty policy set, so an un-governed envelope
/// otherwise yields a sandbox that hangs (no tool/inference/mesh permitted).
/// `kars-default` allows inference/tool/mesh/spawn and denies dangerous shell.
pub const DEFAULT_TOOL_POLICY: &str = "kars-default";

/// Resolve a governance policy for an envelope the orchestrator left
/// un-governed: prefer `kars-default` when the cluster has it, otherwise the
/// first installed policy. Returns `None` only when the cluster has no policies
/// at all (nothing to assign).
pub fn default_tool_policy(o: &crate::routes::options::Options) -> Option<String> {
    if o.tool_policies
        .iter()
        .any(|tp| tp.name == DEFAULT_TOOL_POLICY)
    {
        return Some(DEFAULT_TOOL_POLICY.to_string());
    }
    o.tool_policies.first().map(|tp| tp.name.clone())
}

/// Parse the model's JSON (tolerating code fences / surrounding prose) and
/// Whether a harness runs a PRODUCTIVE one-shot / cadence autonomous session —
/// i.e. it consumes a delivered objective (over the mesh) and returns a real
/// deliverable, with no human chat channel required. Only these may back an
/// autonomous mission or a standing team member.
///
/// - `OpenClaw` — the reference autonomous harness (always-on agent loop).
/// - `Hermes` — verified autonomous: its mesh worker executes a delivered
///   `task_request` in-process and replies (the "idle daemon" boot line is a
///   red herring — the gateway still loads the plugin + worker). Confirmed E2E.
/// - `BYO` — the operator's own image, contractually responsible for consuming
///   the objective + delivering; we take it at its word rather than downgrade it.
///
/// Everything else in the catalogue today (Anthropic / OpenAIAgents / MAF /
/// LangGraph / PydanticAi) is a **bootstrap-only** adapter: it pins a provider
/// URL + OTel and exits, with no task-execution loop, so it delivers NOTHING
/// autonomously. Routing autonomous work to one is a silent no-op — so we
/// correct it to OpenClaw (attested), exactly like the mission path.
pub(crate) fn is_autonomous_harness(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("OpenClaw")
        || kind.eq_ignore_ascii_case("Hermes")
        || kind.eq_ignore_ascii_case("BYO")
}

/// Inverse of [`is_autonomous_harness`]: a harness that cannot run a productive
/// autonomous session (a bootstrap-only SDK/graph adapter today). Autonomous
/// missions and team members routed to one must be corrected/refused.
pub(crate) fn is_non_autonomous_harness(kind: &str) -> bool {
    !kind.trim().is_empty() && !is_autonomous_harness(kind)
}
