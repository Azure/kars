// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::super::{
    GithubDependabotAlert, append_bounded_tasks, dependabot_alert_task, merge_discovered_tasks,
};
use super::tests::{legacy, snapshot};
use super::*;
use std::collections::BTreeMap;

fn finding(number: u64, advisory: &str, fixed: &str, updated_at: &str) -> TeamTaskDto {
    let alert: GithubDependabotAlert = serde_json::from_value(serde_json::json!({
        "number": number,
        "html_url": format!("https://github.com/acme/api/security/dependabot/{number}"),
        "dependency": {
            "package": {"ecosystem": "npm", "name": "vite"},
            "manifest_path": "Services/package-lock.json",
            "scope": "development"
        },
        "security_advisory": {
            "ghsa_id": advisory, "cve_id": null, "summary": "vulnerable input handling",
            "severity": "high"
        },
        "security_vulnerability": {
            "vulnerable_version_range": "< 8",
            "first_patched_version": {"identifier": fixed}
        },
        "updated_at": updated_at
    }))
    .unwrap();
    dependabot_alert_task("acme/api", &alert, "2026-09-14T00:00:00Z")
}

fn first_finding() -> TeamTaskDto {
    finding(17, "GHSA-aaaa-bbbb-cccc", "8.0.0", "2026-09-13T00:00:00Z")
}

fn next_finding() -> TeamTaskDto {
    finding(18, "GHSA-dddd-eeee-ffff", "8.0.1", "2026-09-14T00:00:00Z")
}

fn finished(mut task: TeamTaskDto) -> TeamTaskDto {
    task.status = "done".into();
    task.run = Some(format!("{}-run", task.id));
    task.assignment_nonce = Some("approved-assignment-nonce".into());
    task.done_at = Some("2026-09-14T01:00:00Z".into());
    task.acceptance_criteria = vec!["Approved exact-SHA checks; original input digest".into()];
    task.description
        .push_str("\n\nCompleted PR evidence: https://github.com/acme/api/pull/42");
    task
}

fn sync_poll(
    existing: Vec<TeamTaskDto>,
    tasks: Vec<TeamTaskDto>,
    complete: bool,
) -> (Vec<TeamTaskDto>, usize, usize) {
    let mut known = existing
        .iter()
        .map(|task| (task.id.clone(), task.clone()))
        .collect();
    let incoming =
        remediation_snapshot(tasks, &known, "acme/api", complete, "2026-09-14T02:00:00Z");
    let mut admitted = Vec::new();
    let mut slots = 0;
    assert!(!append_bounded_tasks(
        &mut admitted,
        &mut known,
        incoming,
        &mut slots,
        20
    ));
    let (merged, queued) = merge_discovered_tasks(existing, admitted);
    (merged, queued, slots)
}

fn alert_numbers(task: &TeamTaskDto) -> Vec<u64> {
    observations(&task.description)
        .unwrap()
        .iter()
        .map(|alert| alert.alert_number)
        .collect()
}

#[test]
fn current_producer_new_advisory_after_done_gets_followup_not_lost_or_replayed() {
    let original = finished(first_finding());
    let before = snapshot(&original);
    // The actual current producer deliberately emits the same tuple-scoped v2 ID.
    assert_eq!(original.id, next_finding().id);
    assert!(original.id.starts_with("dependency-remediation-v2-"));
    let (merged, queued, slots) = sync_poll(vec![original], vec![next_finding()], true);
    assert_eq!((queued, slots, merged.len()), (1, 1, 2));
    assert_eq!(snapshot(&merged[0]), before);
    assert_ne!(merged[1].id, merged[0].id);
    assert!(merged[1].id.len() <= 63);
    assert_eq!(merged[1].status, "pending");
    assert!(merged[1].review_required);
    assert!(merged[1].run.is_none());
    assert!(merged[1].assignment_nonce.is_none());
    assert!(merged[1].description.contains(&merged[0].id));
    assert!(
        merged[1]
            .description
            .contains(merged[0].run.as_deref().unwrap())
    );
    assert_eq!(alert_numbers(&merged[1]), vec![18]);
    let (mut merged, queued, slots) = sync_poll(merged, vec![next_finding()], true);
    assert_eq!((queued, slots, merged.len()), (0, 0, 2));
    merged[1] = finished(merged[1].clone());
    let prior = merged.iter().map(snapshot).collect::<Vec<_>>();
    for current_open in [vec![next_finding()], vec![first_finding(), next_finding()]] {
        let (again, queued, slots) = sync_poll(merged.clone(), current_open, true);
        assert_eq!((queued, slots), (0, 0));
        assert_eq!(again.iter().map(snapshot).collect::<Vec<_>>(), prior);
    }
}

#[test]
fn completed_source_ignores_poll_time_titles_and_pr_candidates_but_not_changed_fix_facts() {
    let original = finished(first_finding());
    let before = snapshot(&original);
    let mut unchanged = finding(17, "GHSA-aaaa-bbbb-cccc", "8.0.0", "2026-09-14T02:00:00Z");
    unchanged.title = "Different untrusted display text".into();
    note_candidate_pulls(
        &mut unchanged,
        &[&super::super::tests::pull("agent", "fix-vite", 43)],
    );
    let (merged, queued, slots) = sync_poll(vec![original], vec![unchanged], true);
    assert_eq!((queued, slots, merged.len()), (0, 0, 1));
    assert_eq!(snapshot(&merged[0]), before);
    let changed = finding(17, "GHSA-aaaa-bbbb-cccc", "8.0.2", "2026-09-14T02:00:00Z");
    let (merged, queued, slots) = sync_poll(merged, vec![changed], true);
    assert_eq!((queued, slots, merged.len()), (1, 1, 2));
    assert_eq!(snapshot(&merged[0]), before);
    assert!(merged[1].description.contains("8.0.2"));
}

#[test]
fn current_producer_pending_source_refreshes_without_spending_capacity_or_changing_identity() {
    let original = first_finding();
    let id = original.id.clone();
    let created_at = original.created_at.clone();
    let (merged, queued, slots) = sync_poll(vec![original], vec![next_finding()], true);
    assert_eq!((queued, slots, merged.len()), (0, 0, 1));
    assert_eq!(merged[0].id, id);
    assert_eq!(merged[0].created_at, created_at);
    assert_eq!(alert_numbers(&merged[0]), vec![18]);
    assert!(!merged[0].description.contains("GHSA-aaaa-bbbb-cccc"));
    assert!(merged[0].description.contains("GHSA-dddd-eeee-ffff"));
}

#[test]
fn active_approved_and_nonce_bound_pending_inputs_are_frozen_with_dependent_followups() {
    for status in ["active", "pending", "review", "stuck"] {
        let mut original = finished(first_finding());
        original.status = status.into();
        original.done_at = None;
        original.stuck_since = Some("2026-09-14T00:30:00Z".into());
        if status == "pending" {
            original.run = None;
        }
        let before = snapshot(&original);
        let (merged, queued, slots) = sync_poll(vec![original], vec![next_finding()], true);
        assert_eq!((queued, slots, merged.len()), (1, 1, 2), "{status}");
        assert_eq!(snapshot(&merged[0]), before);
        assert_eq!(merged[1].depends_on, vec![merged[0].id.clone()]);
        assert_eq!(alert_numbers(&merged[1]), vec![18]);
        let again_before = merged.iter().map(snapshot).collect::<Vec<_>>();
        let (again, queued, slots) = sync_poll(merged, vec![next_finding()], true);
        assert_eq!((queued, slots), (0, 0));
        assert_eq!(again.iter().map(snapshot).collect::<Vec<_>>(), again_before);
    }
}

#[test]
fn current_producer_aggregation_is_order_independent_and_removes_only_closed_pending_findings() {
    let (left, queued, slots) = sync_poll(
        Vec::new(),
        vec![next_finding(), first_finding(), first_finding()],
        true,
    );
    let (right, right_queued, right_slots) =
        sync_poll(Vec::new(), vec![first_finding(), next_finding()], true);
    assert_eq!((queued, slots, right_queued, right_slots), (1, 1, 1, 1));
    assert_eq!(
        left.iter().map(snapshot).collect::<Vec<_>>(),
        right.iter().map(snapshot).collect::<Vec<_>>()
    );
    assert_eq!(alert_numbers(&left[0]), vec![17, 18]);
    let (partial, queued, slots) = sync_poll(left.clone(), vec![first_finding()], false);
    assert_eq!((queued, slots), (0, 0));
    assert_eq!(alert_numbers(&partial[0]), vec![17, 18]);
    let (remaining, queued, slots) = sync_poll(left, vec![next_finding()], true);
    assert_eq!((queued, slots), (0, 0));
    assert_eq!(alert_numbers(&remaining[0]), vec![18]);
    assert!(!remaining[0].description.contains("GHSA-aaaa-bbbb-cccc"));
}

#[test]
fn full_empty_snapshot_retires_unassigned_source_without_claiming_delivery_or_erasing_runs() {
    let pending = first_finding();
    let (partial, queued, slots) = sync_poll(vec![pending.clone()], Vec::new(), false);
    assert_eq!((queued, slots), (0, 0));
    assert_eq!(snapshot(&partial[0]), snapshot(&pending));
    let (retired, queued, slots) = sync_poll(vec![pending], Vec::new(), true);
    assert_eq!((queued, slots, retired.len()), (0, 0, 1));
    assert_eq!(retired[0].status, "done");
    assert!(retired[0].run.is_none());
    assert!(
        retired[0]
            .description
            .contains("not evidence of a remediation")
    );
    assert!(alert_numbers(&retired[0]).is_empty());
    let retired_before = snapshot(&retired[0]);
    let (retired, queued, slots) = sync_poll(retired, Vec::new(), true);
    assert_eq!((queued, slots, retired.len()), (0, 0, 1));
    assert_eq!(snapshot(&retired[0]), retired_before);
    for status in ["active", "done"] {
        let mut assigned = finished(first_finding());
        assigned.status = status.into();
        let before = snapshot(&assigned);
        let (merged, queued, slots) = sync_poll(vec![assigned], Vec::new(), true);
        assert_eq!((queued, slots, merged.len()), (0, 0, 1));
        assert_eq!(snapshot(&merged[0]), before);
    }
}

#[test]
fn refreshed_followup_preserves_original_history_and_withdrawn_findings_are_not_reintroduced() {
    let original = finished(first_finding());
    let before = snapshot(&original);
    let (merged, _, _) = sync_poll(vec![original], vec![first_finding(), next_finding()], true);
    assert_eq!(alert_numbers(&merged[1]), vec![18]);
    let updated = finding(18, "GHSA-dddd-eeee-ffff", "8.0.3", "2026-09-14T03:00:00Z");
    let (merged, queued, slots) = sync_poll(merged, vec![updated], true);
    assert_eq!((queued, slots, merged.len()), (0, 0, 2));
    assert_eq!(snapshot(&merged[0]), before);
    assert!(merged[1].description.contains(&merged[0].id));
    assert!(merged[1].description.contains("8.0.3"));
    assert_eq!(alert_numbers(&merged[1]), vec![18]);
    let (merged, queued, slots) = sync_poll(merged, vec![first_finding()], true);
    assert_eq!((queued, slots), (0, 0));
    assert_eq!(merged[1].status, "done");
    assert!(alert_numbers(&merged[1]).is_empty());
}

#[test]
fn proven_legacy_history_remains_immutable_when_current_producer_finds_a_new_alert() {
    let original = legacy(
        "acme/api",
        Some("Services/package-lock.json"),
        "vite",
        "done",
    );
    let before = snapshot(&original);
    let (merged, queued, slots) = sync_poll(vec![original], vec![next_finding()], true);
    assert_eq!((queued, slots, merged.len()), (1, 1, 2));
    assert_eq!(snapshot(&merged[0]), before);
    assert!(merged[1].id.starts_with("dependency-remediation-v2-"));
    assert!(merged[1].description.contains(&merged[0].id));
    let mut pending = legacy(
        "acme/api",
        Some("Services/package-lock.json"),
        "vite",
        "pending",
    );
    pending.run = None;
    pending.assignment_nonce = None;
    let id = pending.id.clone();
    let (merged, queued, slots) = sync_poll(vec![pending], vec![next_finding()], true);
    assert_eq!((queued, slots, merged.len()), (0, 0, 1));
    assert_eq!(merged[0].id, id);
    assert_eq!(stored_identity(&merged[0].description).unwrap().0, id);
    assert_eq!(alert_numbers(&merged[0]), vec![18]);
}

#[test]
fn final_merge_rechecks_assignment_and_completion_after_admission() {
    for status in ["active", "done"] {
        let pending = first_finding();
        let mut known = BTreeMap::from([(pending.id.clone(), pending.clone())]);
        let mut admitted = Vec::new();
        let mut slots = 0;
        assert!(!append_bounded_tasks(
            &mut admitted,
            &mut known,
            vec![next_finding()],
            &mut slots,
            1
        ));
        assert_eq!((slots, admitted.len()), (0, 1));
        let mut raced = finished(pending);
        raced.status = status.into();
        let before = snapshot(&raced);
        let (merged, queued) = merge_discovered_tasks(vec![raced], admitted);
        assert_eq!((queued, merged.len()), (1, 2));
        assert_eq!(snapshot(&merged[0]), before);
        assert_eq!(alert_numbers(&merged[1]), vec![18]);
    }
}

#[test]
fn direct_final_merge_aggregates_current_producer_and_preserves_completed_pr_evidence() {
    let original = finished(first_finding());
    let before = snapshot(&original);
    let (merged, queued) = merge_discovered_tasks(
        vec![original],
        vec![next_finding(), first_finding(), next_finding()],
    );
    assert_eq!((queued, merged.len()), (1, 2));
    assert_eq!(snapshot(&merged[0]), before);
    assert_eq!(alert_numbers(&merged[1]), vec![18]);
    let again_before = merged.iter().map(snapshot).collect::<Vec<_>>();
    let (again, queued) = merge_discovered_tasks(merged, vec![first_finding(), next_finding()]);
    assert_eq!(queued, 0);
    assert_eq!(again.iter().map(snapshot).collect::<Vec<_>>(), again_before);
}

#[test]
fn remediation_queue_cap_retries_new_followups_but_allows_unassigned_refreshes() {
    let completed = finished(first_finding());
    let mut known = BTreeMap::from([(completed.id.clone(), completed.clone())]);
    let mut admitted = Vec::new();
    let mut slots = 0;
    assert!(append_bounded_tasks(
        &mut admitted,
        &mut known,
        vec![next_finding()],
        &mut slots,
        0
    ));
    assert_eq!((slots, admitted.len(), known.len()), (0, 0, 1));
    assert!(!append_bounded_tasks(
        &mut admitted,
        &mut known,
        vec![next_finding()],
        &mut slots,
        1
    ));
    assert_eq!((slots, admitted.len(), known.len()), (1, 1, 2));
    let pending = first_finding();
    let mut known = BTreeMap::from([(pending.id.clone(), pending)]);
    let mut admitted = Vec::new();
    let mut slots = super::super::MAX_ITEMS_PER_SYNC;
    assert!(!append_bounded_tasks(
        &mut admitted,
        &mut known,
        vec![next_finding()],
        &mut slots,
        0
    ));
    assert_eq!(admitted.len(), 1);
    assert_eq!(slots, super::super::MAX_ITEMS_PER_SYNC);
}

#[test]
fn missing_v2_original_metadata_cannot_authorize_overwriting_a_completed_row() {
    let mut original = finished(first_finding());
    original.description = "Historical completed PR: https://github.com/acme/api/pull/42".into();
    let before = snapshot(&original);
    let (merged, queued) = merge_discovered_tasks(vec![original], vec![next_finding()]);
    assert_eq!((queued, merged.len()), (1, 2));
    assert_eq!(snapshot(&merged[0]), before);
}

#[test]
fn legacy_retirement_cannot_rewrite_a_concurrently_assigned_pending_task() {
    let mut original = first_finding();
    original.id = "dependabot-alert-legacy".into();
    original.assignment_nonce = Some("in-flight-assignment".into());
    let before = snapshot(&original);
    let retirement = super::super::intake::legacy_alert_retirement(
        &original.id,
        &next_finding().id,
        "2026-09-14T01:00:00Z",
    );
    let (merged, queued) = merge_discovered_tasks(vec![original], vec![retirement]);
    assert_eq!((queued, merged.len()), (0, 1));
    assert_eq!(snapshot(&merged[0]), before);
}

#[test]
fn duplicated_alert_pages_keep_latest_facts_and_all_pr_candidate_links_in_either_order() {
    let mut older = first_finding();
    note_candidate_pulls(
        &mut older,
        &[&super::super::tests::pull("agent", "fix-vite", 43)],
    );
    let mut newer = finding(17, "GHSA-aaaa-bbbb-cccc", "8.0.2", "2026-09-14T03:00:00Z");
    note_candidate_pulls(
        &mut newer,
        &[&super::super::tests::pull("agent", "fix-vite", 44)],
    );
    let (left, _) = merge_discovered_tasks(Vec::new(), vec![older.clone(), newer.clone()]);
    let (right, _) = merge_discovered_tasks(Vec::new(), vec![newer, older]);
    assert_eq!(snapshot(&left[0]), snapshot(&right[0]));
    assert_eq!(alert_numbers(&left[0]), vec![17]);
    assert!(left[0].description.contains("8.0.2"));
    assert!(left[0].description.contains("/pull/43"));
    assert!(left[0].description.contains("/pull/44"));
    assert_eq!(left[0].status, "pending");
}

#[test]
fn partial_poll_refreshes_seen_alerts_without_discarding_unseen_pending_findings() {
    let (existing, _, _) = sync_poll(Vec::new(), vec![first_finding(), next_finding()], true);
    let changed = finding(17, "GHSA-aaaa-bbbb-cccc", "8.0.4", "2026-09-14T03:00:00Z");
    let (merged, queued, slots) = sync_poll(existing, vec![changed], false);
    assert_eq!((queued, slots, merged.len()), (0, 0, 1));
    assert_eq!(alert_numbers(&merged[0]), vec![17, 18]);
    assert!(merged[0].description.contains("8.0.4"));
}

#[test]
fn source_withdrawal_final_merge_does_not_change_a_newly_assigned_v2_task() {
    let pending = first_finding();
    let mut known = BTreeMap::from([(pending.id.clone(), pending.clone())]);
    let incoming =
        remediation_snapshot(Vec::new(), &known, "acme/api", true, "2026-09-14T04:00:00Z");
    let mut admitted = Vec::new();
    let mut slots = 0;
    assert!(!append_bounded_tasks(
        &mut admitted,
        &mut known,
        incoming,
        &mut slots,
        0
    ));
    let mut assigned = pending;
    assigned.assignment_nonce = Some("concurrent-assignment".into());
    let before = snapshot(&assigned);
    let (merged, queued) = merge_discovered_tasks(vec![assigned], admitted);
    assert_eq!((queued, merged.len()), (0, 1));
    assert_eq!(snapshot(&merged[0]), before);
}
