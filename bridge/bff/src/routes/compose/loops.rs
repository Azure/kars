// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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

fn pattern_allowed(pattern: &str, surface: &str) -> bool {
    LOOP_PATTERN_IDS.contains(&pattern)
        && match pattern {
            "standing-watch" => surface == "team",
            "explore-branch" => surface == "mission",
            _ => true,
        }
}

/// Keyword heuristic used both to seed the orchestrator and as the fallback.
fn heuristic_pattern(intent: &str, surface: &str) -> &'static str {
    let t = intent.to_ascii_lowercase();
    if surface == "team" {
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

fn parse_proposal(raw: &str, intent: &str, surface: &str) -> Option<ProposeLoopResponse> {
    let value = extract_json_object(raw)?;
    let pattern = value
        .get("pattern")?
        .as_str()
        .filter(|pattern| pattern_allowed(pattern, surface))?;
    Some(ProposeLoopResponse {
        pattern: pattern.to_string(),
        goal: value
            .get("goal")
            .and_then(|goal| goal.as_str())
            .map(str::trim)
            .filter(|goal| !goal.is_empty())
            .unwrap_or(intent)
            .to_string(),
        criteria: value
            .get("criteria")
            .and_then(|criteria| criteria.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        rationale: value
            .get("rationale")
            .and_then(|rationale| rationale.as_str())
            .unwrap_or("Best fit for this intent.")
            .trim()
            .to_string(),
        source: "orchestrator".into(),
    })
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

    let allowed_patterns: Vec<_> = LOOP_PATTERN_IDS
        .iter()
        .copied()
        .filter(|pattern| pattern_allowed(pattern, surface))
        .collect();
    let system = format!(
        "You are a loop-engineering orchestrator. Given a user's intent, pick the ONE feedback-loop \
         pattern that best fits and draft the loop. Patterns: react (reason+act with tools), reflect \
         (draft, self-critique, revise), plan-execute (plan then do), eval-iterate (define acceptance \
         checks first, loop until they pass), explore-branch (generate candidates, prune), \
         standing-watch (periodic observe->detect change->act, teams only). \
         Choose only from the allowed ids for this {surface} surface below. \
         Respond with ONLY a JSON object: {{\"pattern\": one of [{}], \"goal\": string, \
         \"criteria\": string with one success criterion per line, \"rationale\": one short sentence}}.",
        allowed_patterns.join(", ")
    );
    let user = format!(
        "Surface: {surface}\nIntent:\n{intent}\n\nA reasonable default pattern is '{heuristic}', but \
         choose the best fit. Respond with ONLY the JSON object."
    );

    // Ask the orchestrator; parse its JSON. Any failure → honest heuristic.
    let fallback_reason = match orchestrator_complete(
        cluster,
        &system,
        &user,
        &default_model,
        LOOP_COMPOSE_MAX_TOKENS,
    )
    .await
    {
        Ok((raw, _src)) => {
            if let Some(proposal) = parse_proposal(&raw, intent, surface) {
                return Ok(Json(proposal));
            }
            tracing::warn!(
                surface,
                "Orchestrator returned an invalid or unsupported loop"
            );
            "the orchestrator returned an invalid or unsupported loop for this surface"
        }
        Err(error) => {
            tracing::warn!(%error, surface, "Loop orchestration unavailable");
            "the orchestrator was unavailable"
        }
    };

    Ok(Json(ProposeLoopResponse {
        pattern: heuristic.to_string(),
        goal: intent.to_string(),
        criteria: String::new(),
        rationale: format!("Suggested from your intent's keywords because {fallback_reason}."),
        source: "heuristic".into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_patterns_match_the_surface_catalogue() {
        for pattern in LOOP_PATTERN_IDS {
            assert_eq!(
                pattern_allowed(pattern, "mission"),
                pattern != "standing-watch"
            );
            assert_eq!(
                pattern_allowed(pattern, "team"),
                pattern != "explore-branch"
            );
        }
        assert!(!pattern_allowed("unknown", "mission"));
        assert!(!pattern_allowed("unknown", "team"));
    }

    #[test]
    fn mission_fallback_never_proposes_a_standing_only_loop() {
        for intent in [
            "Monitor once",
            "Watch every report",
            "Keep an eye on this task",
            "On cadence",
        ] {
            assert!(pattern_allowed(
                heuristic_pattern(intent, "mission"),
                "mission"
            ));
            assert_eq!(heuristic_pattern(intent, "team"), "standing-watch");
        }
    }

    #[test]
    fn incompatible_or_invalid_model_choices_are_not_labelled_orchestrator_proposals() {
        for (surface, pattern) in [
            ("mission", "standing-watch"),
            ("team", "explore-branch"),
            ("mission", "unknown"),
        ] {
            let raw = serde_json::json!({"pattern": pattern, "goal": "Review clinic operations"})
                .to_string();
            assert!(parse_proposal(&raw, "original", surface).is_none());
        }
        assert!(parse_proposal("{}", "original", "mission").is_none());
        assert!(parse_proposal("not JSON", "original", "mission").is_none());
    }

    #[test]
    fn valid_model_proposals_retain_their_review_fields() {
        let raw = r#"{"pattern":"reflect","goal":" Review clinic operations ","criteria":" Check breaks ","rationale":" Critique before approval "}"#;
        let proposal = parse_proposal(raw, "original", "mission").unwrap();
        assert_eq!(proposal.pattern, "reflect");
        assert_eq!(proposal.goal, "Review clinic operations");
        assert_eq!(proposal.criteria, "Check breaks");
        assert_eq!(proposal.rationale, "Critique before approval");
        assert_eq!(proposal.source, "orchestrator");
    }
}
