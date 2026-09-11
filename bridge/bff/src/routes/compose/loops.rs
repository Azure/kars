use axum::Json;
use axum::extract::State;
use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::routes::options::build_options;
use crate::state::AppState;

use super::client::{extract_json_object, orchestrator_complete};

const LOOP_COMPOSE_MAX_TOKENS: u32 = 1_200;

/// `POST /api/namespaces/:ns/propose-loop` — the orchestrator turns a raw intent
/// into a PROPOSED loop (2026 loop engineering): it picks the feedback-loop
/// pattern that fits and drafts the goal + success criteria. The web then shows
/// this in the Loop Designer for the user to REVIEW and tweak before executing —
/// so the loop is orchestrator-defined, human-reviewed, then run. Falls back to a
/// keyword heuristic when the orchestrator is unreachable (never a dead end).
#[derive(Debug, serde::Deserialize)]
pub struct ProposeLoopRequest {
    pub intent: String,
    /// "mission" (single run) or "team" (standing cadence loop).
    #[serde(default)]
    pub surface: String,
}

#[derive(Debug, Serialize)]
pub struct ProposeLoopResponse {
    /// Chosen loop pattern id (matches the web catalog: react, reflect,
    /// plan-execute, eval-iterate, explore-branch, standing-watch).
    pub pattern: String,
    /// The goal the orchestrator distilled from the intent.
    pub goal: String,
    /// Draft success criteria (one per line).
    pub criteria: String,
    /// One-line why-this-pattern rationale.
    pub rationale: String,
    /// "orchestrator" when the model chose it, "heuristic" on fallback.
    pub source: String,
}

const LOOP_PATTERN_IDS: [&str; 6] = [
    "react",
    "reflect",
    "plan-execute",
    "eval-iterate",
    "explore-branch",
    "standing-watch",
];

/// Keyword heuristic used both to seed the orchestrator and as the fallback.
fn heuristic_pattern(intent: &str, surface: &str) -> &'static str {
    let t = intent.to_ascii_lowercase();
    if surface == "team"
        || t.contains("watch")
        || t.contains("monitor")
        || t.contains("keep an eye")
        || t.contains("on cadence")
        || t.contains("every ")
    {
        return "standing-watch";
    }
    if t.contains("test")
        || t.contains("verify")
        || t.contains("pass")
        || t.contains("acceptance")
        || t.contains("ci")
    {
        return "eval-iterate";
    }
    if t.contains("research")
        || t.contains("investigate")
        || t.contains("browse")
        || t.contains("search")
        || t.contains("find ")
    {
        return "react";
    }
    if t.contains("write")
        || t.contains("draft")
        || t.contains("report")
        || t.contains("polish")
        || t.contains("review")
    {
        return "reflect";
    }
    if t.contains("design")
        || t.contains("compare")
        || t.contains("options")
        || t.contains("approach")
        || t.contains("brainstorm")
    {
        return "explore-branch";
    }
    "plan-execute"
}

pub async fn propose_loop(
    State(state): State<AppState>,
    Json(req): Json<ProposeLoopRequest>,
) -> AppResult<Json<ProposeLoopResponse>> {
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let intent = req.intent.trim();
    if intent.is_empty() {
        return Err(AppError::BadRequest("intent is required".into()));
    }
    let surface = if req.surface == "team" {
        "team"
    } else {
        "mission"
    };
    let heuristic = heuristic_pattern(intent, surface);

    let options = build_options(cluster).await?;
    // Don't fabricate a specific model when none is configured — an empty model
    // makes the orchestrator call fail cleanly and fall back to the heuristic
    // below, rather than pretending a named model exists on this cluster.
    let default_model = options
        .models
        .iter()
        .find(|m| m.is_default)
        .or_else(|| options.models.first())
        .map(|m| m.deployment.clone())
        .unwrap_or_default();

    let system = format!(
        "You are a loop-engineering orchestrator. Given a user's intent, pick the ONE feedback-loop \
         pattern that best fits and draft the loop. Patterns: react (reason+act with tools), reflect \
         (draft, self-critique, revise), plan-execute (plan then do), eval-iterate (define acceptance \
         checks first, loop until they pass), explore-branch (generate candidates, prune), \
         standing-watch (periodic observe->detect change->act, for standing {surface} work). \
         Respond with ONLY a JSON object: {{\"pattern\": one of [{}], \"goal\": string, \
         \"criteria\": string with one success criterion per line, \"rationale\": one short sentence}}.",
        LOOP_PATTERN_IDS.join(", ")
    );
    let user = format!(
        "Surface: {surface}\nIntent:\n{intent}\n\nA reasonable default pattern is '{heuristic}', but \
         choose the best fit. Respond with ONLY the JSON object."
    );

    // Ask the orchestrator; parse its JSON. Any failure → honest heuristic.
    match orchestrator_complete(
        cluster,
        &system,
        &user,
        &default_model,
        LOOP_COMPOSE_MAX_TOKENS,
    )
    .await
    {
        Ok((raw, _src)) => {
            if let Some(v) = extract_json_object(&raw) {
                let pattern = v
                    .get("pattern")
                    .and_then(|p| p.as_str())
                    .filter(|p| LOOP_PATTERN_IDS.contains(p))
                    .unwrap_or(heuristic)
                    .to_string();
                let goal = v
                    .get("goal")
                    .and_then(|g| g.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| intent.to_string());
                let criteria = v
                    .get("criteria")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let rationale = v
                    .get("rationale")
                    .and_then(|r| r.as_str())
                    .unwrap_or("Best fit for this intent.")
                    .trim()
                    .to_string();
                return Ok(Json(ProposeLoopResponse {
                    pattern,
                    goal,
                    criteria,
                    rationale,
                    source: "orchestrator".into(),
                }));
            }
            // Unparseable model output → heuristic.
        }
        Err(_) => { /* orchestrator unreachable → heuristic */ }
    }

    Ok(Json(ProposeLoopResponse {
        pattern: heuristic.to_string(),
        goal: intent.to_string(),
        criteria: String::new(),
        rationale: "Chosen from your intent's keywords (orchestrator unavailable).".into(),
        source: "heuristic".into(),
    }))
}
