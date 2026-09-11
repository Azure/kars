use crate::routes::options::ModelOption;

pub(super) fn orchestrator_quality_score(deployment: &str) -> Option<i64> {
    let model = deployment.to_ascii_lowercase().replace(['.', '_'], "-");
    if model.contains("embedding")
        || model.contains("image")
        || model.contains("flux")
        || model.contains("dall-e")
    {
        return None;
    }
    let score = if model.contains("gpt-5-6") || model.contains("gpt-5.6") {
        1_000
    } else if model.contains("claude-opus-4-8") {
        990
    } else if model.contains("claude-opus-4-7") {
        980
    } else if model.contains("gpt-5-4-pro") {
        970
    } else if model.contains("gpt-5-4") {
        950
    } else if model.contains("claude-sonnet-5") {
        940
    } else if model.contains("gpt-4-1") {
        900
    } else if model.contains("gpt-oss-120b") {
        850
    } else if model.contains("gpt-5") || model.contains("claude") {
        800
    } else {
        500
    };
    Some(score)
}

fn catalogue_has_model(models: &[ModelOption], provider: &str, deployment: &str) -> bool {
    models
        .iter()
        .any(|model| model.provider == provider && model.deployment == deployment)
}

pub(super) fn catalogue_has_model_key(models: &[ModelOption], key: &str) -> bool {
    key.split_once("::")
        .is_some_and(|(provider, deployment)| catalogue_has_model(models, provider, deployment))
}

pub(super) fn recommendation_is_actionable(
    recommended: Option<&str>,
    low_confidence: bool,
) -> bool {
    recommended.is_some() && !low_confidence
}

pub(super) fn select_orchestrator_route(
    options: &crate::routes::options::Options,
    efficiency: &crate::routes::efficiency::EfficiencyDto,
) -> Option<(String, String, String)> {
    let actionable_recommendation = efficiency.recommended.as_deref().filter(|_| {
        recommendation_is_actionable(
            efficiency.recommended.as_deref(),
            efficiency.recommended_low_confidence,
        )
    });
    options
            .models
            .iter()
            .filter_map(|model| {
                let quality = orchestrator_quality_score(&model.deployment)?;
                let route = efficiency
                    .routes
                    .iter()
                    .find(|route| route.route == model.deployment);
                let frontier_bonus = if actionable_recommendation == Some(model.deployment.as_str()) {
                    80
                } else {
                    0
                };
                let evidence_bonus = route
                    .map(|route| (route.acceptance_rate * 50.0).round() as i64)
                    .unwrap_or(0);
                Some((quality + frontier_bonus + evidence_bonus, model))
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, model)| {
                let basis = if actionable_recommendation == Some(model.deployment.as_str()) {
                    format!(
                        "Selected {} from {} as the strongest orchestration-capable model and current efficiency-frontier recommendation.",
                        model.deployment, model.provider
                    )
                } else {
                    format!(
                        "Selected {} from {} as the strongest orchestration-capable model in the configured catalogue.",
                        model.deployment, model.provider
                    )
                };
                (model.provider.clone(), model.deployment.clone(), basis)
            })
}

/// A one-line, plain-language basis for recommending `deployment`, drawn from
/// the learned efficiency frontier — real accepted-outcome counts, never
/// fabricated. Falls back to a generic line if the route has no stats yet.
pub(super) fn efficiency_basis(
    eff: &crate::routes::efficiency::EfficiencyDto,
    deployment: &str,
) -> String {
    if let Some(r) = eff.routes.iter().find(|r| r.route == deployment) {
        let acc = (r.acceptance_rate * 100.0).round() as i64;
        // Distinguish a CONFIDENT recommendation (enough runs, a real acceptance
        // rate) from the best of a sparse/weak set. Without this, a route that is
        // merely "least-bad" — e.g. 7% accepted over a handful of runs — read as a
        // glowing endorsement next to the word "Recommended", which is dishonest.
        let strong = r.runs >= 5 && r.acceptance_rate >= 0.5;
        let mut s = if strong {
            format!(
                "Best learned route — {acc}% accepted over {} run{}",
                r.runs,
                if r.runs == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Best available route so far (limited signal) — {acc}% accepted over {} run{}",
                r.runs,
                if r.runs == 1 { "" } else { "s" }
            )
        };
        if !r.harness.is_empty() {
            s.push_str(&format!(" on {}", r.harness));
        }
        // Reliability of 0% over a tiny sample is "not yet established", not a
        // meaningful "0%" — report it honestly so it doesn't read as "0% reliable".
        match (r.reliability_rate, r.reliability_k, r.reliability_samples) {
            (Some(rel), Some(k), samples) if samples >= 3 && rel > 0.0 => {
                s.push_str(&format!(
                    ", pass^{k} reliability {}%",
                    (rel * 100.0).round() as i64
                ));
            }
            (Some(_), _, samples) => {
                s.push_str(&format!(", reliability not yet established (n={samples})"));
            }
            _ => {}
        }
        if let Some(usd) = r.usd_per_outcome {
            s.push_str(&format!(", ${usd:.2}/outcome"));
        }
        s.push('.');
        s
    } else {
        "Recommended by the learned efficiency frontier.".to_string()
    }
}

pub(super) fn should_strengthen_team_principal(
    role_count: usize,
    current_deployment: &str,
    strongest_deployment: &str,
) -> bool {
    if role_count < 3 || current_deployment == strongest_deployment {
        return false;
    }
    let current = orchestrator_quality_score(current_deployment).unwrap_or(0);
    let strongest = orchestrator_quality_score(strongest_deployment).unwrap_or(0);
    current < 900 && strongest >= 950
}

pub(super) fn efficient_member_route_is_qualified(runs: i64, acceptance_rate: f64) -> bool {
    // A lower-cost member route needs repeated evidence before a newly composed
    // team inherits it. This is intentionally stricter on sample count than a
    // descriptive efficiency-basis label because it changes live execution.
    runs >= 3 && acceptance_rate >= 0.67
}
