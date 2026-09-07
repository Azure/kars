// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::kars_task::{KarsTaskSpec, TaskEnvelope};
use crate::kars_team::{KarsTeam, KarsTeamSpec, TeamCadence};
use crate::kars_team_reconciler::specs;
use serde_json::Value;
use std::collections::BTreeMap;

fn team() -> KarsTeam {
    KarsTeam::new(
        "engineering",
        KarsTeamSpec {
            charter: "c".repeat(160),
            envelope: TaskEnvelope {
                tier: 4,
                authority_ceiling: 3,
                delegation_depth: 2,
                ..Default::default()
            },
            cadence: Some(TeamCadence {
                every_minutes: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
}

fn history(count: usize, content: &str) -> (ConfigMap, Vec<CommonsEntry>) {
    let mut data = BTreeMap::new();
    let mut entries = Vec::new();
    for n in 0..count {
        let id = format!("00000000-0000-4000-8000-{n:012}");
        data.insert(content_key(&id), content.into());
        entries.push(CommonsEntry {
            id,
            title: "t".repeat(160),
            author: format!("engineering-run-{n:032x}"),
            source_task: format!("engineering-run-{n:032x}"),
            created_at: "2026-09-07T12:00:00Z".into(),
            digest: format!("sha256:{}", "a".repeat(32)),
            size_bytes: content.len() as i64,
        });
    }
    (
        ConfigMap {
            data: Some(data),
            ..Default::default()
        },
        entries,
    )
}

fn references(prompt: &str) -> Vec<Value> {
    if prompt.is_empty() {
        return Vec::new();
    }
    let body = prompt
        .strip_prefix(HEADER)
        .unwrap()
        .strip_suffix(FOOTER)
        .unwrap();
    assert_eq!(prompt.matches(FOOTER).count(), 1);
    body.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn five_ordinary_entries_fit_the_entire_objective_not_just_snippets() {
    let team = team();
    let (cm, entries) = history(5, &"f".repeat(400));
    let original_history = prior_knowledge(&cm, &entries, usize::MAX).unwrap();
    let fixed = specs::run_spec(&team, "").unwrap().objective;
    assert!(fixed.chars().count() + original_history.chars().count() > specs::MAX_OBJECTIVE_CHARS);
    assert!(specs::run_spec(&team, &original_history).is_err());

    let allowance = specs::run_knowledge_budget(&team).unwrap();
    let bounded_history = prior_knowledge(&cm, &entries, allowance).unwrap();
    let run = specs::run_spec(&team, &bounded_history).unwrap();
    assert!(run.objective.starts_with(&fixed));
    assert!(run.objective.chars().count() <= specs::MAX_OBJECTIVE_CHARS);
    let selected = references(&bounded_history);
    assert!(!selected.is_empty() && selected.len() < entries.len());
    assert_eq!(selected[0]["id"], entries.last().unwrap().id);
    assert!(
        selected
            .iter()
            .all(|entry| entry["content"].as_str().unwrap().chars().count() == 400)
    );
    assert_eq!(team.spec.charter, "c".repeat(160));
}

#[test]
fn unicode_and_json_escaping_are_budgeted_after_reference_serialization() {
    let mut team = team();
    team.spec.display_name = Some("研究チーム🦀".into());
    team.spec.charter = "調査🦀".repeat(80);
    let content: String = r#"東京🦀 "quote" C:\notes\run "#.repeat(30).chars().take(400).collect();
    let (cm, entries) = history(2, &content);
    let all = prior_knowledge(&cm, &entries, usize::MAX).unwrap();
    let first = all.lines().find(|line| line.starts_with('{')).unwrap();
    assert!(first.contains(r#"\""#) && first.contains(r#"\\"#));
    let allowance = HEADER.chars().count() + FOOTER.chars().count() + first.chars().count() + 1;
    let bounded = prior_knowledge(&cm, &entries, allowance).unwrap();
    assert_eq!(bounded.chars().count(), allowance);
    assert!(
        bounded.len() > allowance,
        "Unicode bytes are not CEL characters"
    );
    assert_eq!(references(&bounded).len(), 1);

    let run = specs::run_spec(&team, &bounded).unwrap();
    let wire = serde_json::to_string(&run).unwrap();
    let decoded: KarsTaskSpec = serde_json::from_str(&wire).unwrap();
    assert_eq!(decoded.objective, run.objective);
    assert!(decoded.objective.chars().count() <= specs::MAX_OBJECTIVE_CHARS);
    assert!(
        prior_knowledge(&cm, &entries, allowance - 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn oversized_fixed_prefix_is_rejected_without_truncating_the_charter() {
    let mut team = team();
    team.spec.charter = "x".repeat(specs::MAX_OBJECTIVE_CHARS);
    let before = team.spec.charter.clone();
    let error = specs::run_knowledge_budget(&team).unwrap_err();
    assert!(error.contains("fixed prefix") && error.contains("4096"));
    assert!(specs::run_spec(&team, "").is_err());
    assert!(
        team.validation_errors()
            .iter()
            .any(|error| error.contains("fixed prefix"))
    );
    assert_eq!(team.spec.charter, before);
}

#[test]
fn zero_available_history_keeps_the_full_fixed_objective_and_no_fragments() {
    let mut team = team();
    team.spec.charter.clear();
    let charter_space = specs::run_knowledge_budget(&team).unwrap();
    team.spec.charter = "🦀".repeat(charter_space);
    assert_eq!(specs::run_knowledge_budget(&team).unwrap(), 0);
    let (cm, entries) = history(5, &"f".repeat(400));
    let prior = prior_knowledge(&cm, &entries, 0).unwrap();
    assert!(prior.is_empty());
    let run = specs::run_spec(&team, &prior).unwrap();
    assert_eq!(run.objective.chars().count(), specs::MAX_OBJECTIVE_CHARS);
    assert!(run.objective.ends_with(&team.spec.charter));
    assert!(specs::run_spec(&team, "x").is_err());
}

#[test]
fn partial_history_contains_only_whole_entries_and_complete_framing() {
    let (cm, entries) = history(3, &"f".repeat(400));
    let all = prior_knowledge(&cm, &entries, usize::MAX).unwrap();
    let entry_size = all
        .lines()
        .find(|line| line.starts_with('{'))
        .unwrap()
        .chars()
        .count()
        + 1;
    let framing = HEADER.chars().count() + FOOTER.chars().count();
    for budget in [
        0,
        framing - 1,
        framing,
        framing + entry_size - 1,
        framing + entry_size,
        framing + 2 * entry_size,
    ] {
        let prompt = prior_knowledge(&cm, &entries, budget).unwrap();
        assert!(prompt.chars().count() <= budget);
        let selected = references(&prompt);
        assert_eq!(selected.len(), budget.saturating_sub(framing) / entry_size);
        for reference in selected {
            for key in [
                "id",
                "title",
                "author",
                "sourceTask",
                "createdAt",
                "digest",
                "content",
            ] {
                assert!(reference[key].is_string(), "{key}");
            }
        }
    }
}

#[test]
fn a_large_newer_entry_does_not_exclude_an_older_entry_that_fits() {
    let (mut cm, entries) = history(2, &"f".repeat(400));
    cm.data
        .as_mut()
        .unwrap()
        .insert(content_key(&entries[0].id), "small".into());
    let old_only = prior_knowledge(&cm, &entries[..1], usize::MAX).unwrap();
    let bounded = prior_knowledge(&cm, &entries, old_only.chars().count()).unwrap();
    let selected = references(&bounded);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["id"], entries[0].id);
    assert_eq!(selected[0]["content"], "small");
}

#[test]
fn objective_budget_remains_aligned_with_existing_admission_limit() {
    let rule = format!(
        "size(self.objective) > 0 && size(self.objective) <= {}",
        specs::MAX_OBJECTIVE_CHARS,
    );
    assert!(
        crate::crd_validations::kars_task_validations()
            .iter()
            .any(|validation| validation.rule == rule)
    );
}
