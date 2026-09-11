use super::super::{
    GithubDependabotAlert, append_bounded_tasks, dependabot_alert_task, merge_discovered_tasks,
};
use super::*;
use std::collections::BTreeMap;

fn discovered(repo: &str, path: Option<&str>, package: &str, number: u64) -> TeamTaskDto {
    let alert: GithubDependabotAlert = serde_json::from_value(serde_json::json!({
        "number": number,
        "html_url": format!("https://github.com/{repo}/security/dependabot/{number}"),
        "dependency": {
            "package": {"ecosystem": "npm", "name": package},
            "manifest_path": path
        },
        "security_vulnerability": {"vulnerable_version_range": "< 8"}
    }))
    .unwrap();
    dependabot_alert_task(repo, &alert, "2026-09-11T00:00:00Z")
}

fn legacy(repo: &str, path: Option<&str>, package: &str, status: &str) -> TeamTaskDto {
    let mut task = discovered(repo, path, package, 17);
    let id = RemediationIdentity::new(repo, path, package).legacy_work_id();
    task.description = task.description.replace(&task.id, &id);
    task.id = id;
    task.status = status.into();
    task.run = Some("original-remediation-run".into());
    task.assignment_nonce = Some("original-assignment".into());
    task.depends_on = vec!["original-pr-assessment".into()];
    task.acceptance_criteria = vec!["Preserve exact-SHA verification receipt".into()];
    task.done_at = (status == "done").then(|| "2026-09-11T01:00:00Z".into());
    task.stuck_since = (status == "stuck").then(|| "2026-09-11T00:30:00Z".into());
    task
}

fn snapshot(task: &TeamTaskDto) -> serde_json::Value {
    serde_json::to_value(task).unwrap()
}

#[test]
fn v2_ids_are_stable_repo_normalized_and_kubernetes_bounded() {
    let expected = "dependency-remediation-v2-f06d77fbff81b7207777";
    assert_eq!(
        remediation_work_id(
            "Acme/API",
            Some("Services/package-lock.json"),
            "@babel/core"
        ),
        expected
    );
    assert_eq!(
        remediation_work_id(
            "acme/api",
            Some("Services/package-lock.json"),
            "@babel/core"
        ),
        expected
    );
    assert_eq!(
        remediation_work_id("acme/api", None, "@babel/core"),
        "dependency-remediation-v2-6a4b58166374d7af50e5"
    );
    let long_path = "Services/".repeat(200);
    let id = remediation_work_id("acme/api", Some(&long_path), "@babel/core");
    assert_eq!(id.len(), expected.len());
    assert!(id.len() <= 63);
    assert!(
        id.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    );
}

#[test]
fn v2_identity_preserves_git_path_and_package_case() {
    let upper = discovered("acme/api", Some("Services/package-lock.json"), "vite", 17);
    let lower = discovered("acme/api", Some("services/package-lock.json"), "vite", 18);
    assert_ne!(upper.id, lower.id);
    assert_eq!(
        upper.id,
        remediation_work_id("acme/api", Some("Services/package-lock.json"), "vite")
    );
    assert_ne!(
        remediation_work_id("acme/api", Some("pom.xml"), "Artifact"),
        remediation_work_id("acme/api", Some("pom.xml"), "artifact")
    );
    let (merged, queued) = merge_discovered_tasks(Vec::new(), vec![upper, lower]);
    assert_eq!(queued, 2);
    assert_eq!(merged.len(), 2);
}

#[test]
fn title_only_pr_candidates_cannot_complete_or_suppress_distinct_manifest_work() {
    let candidate_pr = super::super::tests::pull("agent", "fix-vite", 42);
    let mut tasks = vec![
        discovered("acme/api", Some("Services/package-lock.json"), "vite", 17),
        discovered("acme/api", Some("services/package-lock.json"), "vite", 18),
    ];
    for task in &mut tasks {
        note_candidate_pulls(task, &[&candidate_pr]);
        assert_eq!(task.status, "pending");
        assert!(task.done_at.is_none());
        assert!(task.run.is_none());
        assert!(
            task.description
                .contains("their titles are not evidence of coverage")
        );
        assert!(task.description.contains("exact case-sensitive manifest"));
    }
    let mut admitted = Vec::new();
    let mut known = BTreeMap::new();
    let mut slots = 0;
    assert!(!append_bounded_tasks(
        &mut admitted,
        &mut known,
        tasks,
        &mut slots,
        2
    ));
    assert_eq!(slots, 2);
    let (merged, queued) = merge_discovered_tasks(Vec::new(), admitted);
    assert_eq!(queued, 2);
    assert_eq!(merged.len(), 2);
    assert!(merged.iter().all(|task| task.status == "pending"));
}

#[test]
fn structured_framing_distinguishes_null_empty_unknown_and_delimiters() {
    let pairs = [
        ((None, "pkg"), (Some("unknown"), "pkg")),
        ((None, "pkg"), (Some(""), "pkg")),
        ((Some("a:b"), "c"), (Some("a"), "b:c")),
        ((Some("a\"/b"), "c"), (Some("a"), "\"/b:c")),
    ];
    for ((left_path, left_package), (right_path, right_package)) in pairs {
        assert_ne!(
            remediation_work_id("acme/api", left_path, left_package),
            remediation_work_id("acme/api", right_path, right_package)
        );
    }
    assert_eq!(
        RemediationIdentity::new("acme/api", Some("a:b"), "c").legacy_work_id(),
        RemediationIdentity::new("acme/api", Some("a"), "b:c").legacy_work_id()
    );
}

#[test]
fn proven_legacy_work_resumes_without_changing_state_or_existing_links() {
    for status in ["pending", "active", "done", "stuck"] {
        let original = legacy(
            "Acme/API",
            Some("Services/package-lock.json"),
            "vite",
            status,
        );
        let mut linked = discovered("acme/api", Some("consumer/package-lock.json"), "vite", 19);
        linked.depends_on = vec![original.id.clone()];
        let before = vec![snapshot(&original), snapshot(&linked)];
        let candidate = discovered("acme/api", Some("Services/package-lock.json"), "vite", 99);
        let (merged, queued) = merge_discovered_tasks(vec![original, linked], vec![candidate]);
        assert_eq!(queued, 0, "{status}");
        assert_eq!(merged.iter().map(snapshot).collect::<Vec<_>>(), before);
    }
}

#[test]
fn legacy_explicit_null_manifest_can_resume_but_unknown_is_distinct() {
    let original = legacy("acme/api", None, "vite", "done");
    let before = snapshot(&original);
    let (merged, queued) = merge_discovered_tasks(
        vec![original],
        vec![discovered("ACME/API", None, "vite", 99)],
    );
    assert_eq!(queued, 0);
    assert_eq!(snapshot(&merged[0]), before);
    let (merged, queued) = merge_discovered_tasks(
        merged,
        vec![discovered("acme/api", Some("unknown"), "vite", 100)],
    );
    assert_eq!(queued, 1);
    assert_eq!(merged.len(), 2);
    assert_eq!(snapshot(&merged[0]), before);
    assert_eq!(merged[1].status, "pending");
}

#[test]
fn distinct_completed_legacy_manifest_does_not_swallow_new_work() {
    for (original_path, new_path) in [
        ("Services/package-lock.json", "services/package-lock.json"),
        ("services/package-lock.json", "Services/package-lock.json"),
    ] {
        let original = legacy("acme/api", Some(original_path), "vite", "done");
        let before = snapshot(&original);
        let candidate = discovered("acme/api", Some(new_path), "vite", 18);
        let candidate_before = snapshot(&candidate);
        let (merged, queued) = merge_discovered_tasks(vec![original], vec![candidate.clone()]);
        assert_eq!(queued, 1);
        assert_eq!(merged.len(), 2);
        assert_eq!(snapshot(&merged[0]), before);
        assert_eq!(snapshot(&merged[1]), candidate_before);
        let (repeated, queued) = merge_discovered_tasks(merged, vec![candidate]);
        assert_eq!(queued, 0);
        assert_eq!(repeated.len(), 2);
        assert_eq!(snapshot(&repeated[0]), before);
    }
}

#[test]
fn colon_colliding_legacy_work_does_not_absorb_a_different_framed_identity() {
    let original = legacy("acme/api", Some("a:b"), "c", "done");
    let before = snapshot(&original);
    let candidate = discovered("acme/api", Some("a"), "b:c", 18);
    let (merged, queued) = merge_discovered_tasks(vec![original], vec![candidate]);
    assert_eq!(queued, 1);
    assert_eq!(merged.len(), 2);
    assert_eq!(snapshot(&merged[0]), before);
    assert_eq!(merged[1].status, "pending");
}

#[test]
fn incomplete_or_unattributed_legacy_metadata_preserves_history_and_warns() {
    let original = legacy("acme/api", Some("package-lock.json"), "vite", "done");
    let complete = serde_json::json!({
        "signal": "dependabot_alert",
        "work_id": original.id,
        "repo": "acme/api",
        "details": {"manifest_path": "package-lock.json", "package": "vite"}
    });
    let mut missing_manifest = complete.clone();
    missing_manifest["details"]
        .as_object_mut()
        .unwrap()
        .remove("manifest_path");
    let mut missing_repo = complete.clone();
    missing_repo.as_object_mut().unwrap().remove("repo");
    let mut wrong_work_id = complete.clone();
    wrong_work_id["work_id"] = serde_json::json!("unrelated-task");
    let mut wrong_signal = complete.clone();
    wrong_signal["signal"] = serde_json::json!("code_scanning_alert");
    let mut wrong_manifest_type = complete;
    wrong_manifest_type["details"]["manifest_path"] = serde_json::json!(42);
    let descriptions = [
        "Historical result without original source metadata".into(),
        "repo=acme/api; manifest=package-lock.json; pkg=vite;".into(),
        format!("{SOURCE_MARKER}{missing_manifest}"),
        format!("{SOURCE_MARKER}{missing_repo}"),
        format!("{SOURCE_MARKER}{wrong_work_id}"),
        format!("{SOURCE_MARKER}{wrong_signal}"),
        format!("{SOURCE_MARKER}{wrong_manifest_type}"),
        format!("{SOURCE_MARKER}{{malformed"),
    ];
    for description in descriptions {
        let mut original = original.clone();
        original.description = description;
        let before = snapshot(&original);
        let candidate = discovered("acme/api", Some("package-lock.json"), "vite", 99);
        let (merged, queued) = merge_discovered_tasks(vec![original], vec![candidate.clone()]);
        assert_eq!(queued, 1);
        assert_eq!(merged.len(), 2);
        assert_eq!(snapshot(&merged[0]), before);
        assert_eq!(merged[1].status, "pending");
        assert_eq!(merged[1].id, candidate.id);
        assert!(
            merged[1]
                .description
                .contains("Remediation identity ambiguity:")
        );
        assert!(merged[1].description.contains("not evidence of delivery"));
        let (repeated, queued) = merge_discovered_tasks(merged, vec![candidate]);
        assert_eq!(queued, 0);
        assert_eq!(repeated.len(), 2);
        assert_eq!(snapshot(&repeated[0]), before);
        assert_eq!(
            repeated[1]
                .description
                .matches("Remediation identity ambiguity:")
                .count(),
            1
        );
    }
}

#[test]
fn matching_reads_original_json_not_case_folded_or_mixed_prose() {
    let mut task = legacy(
        "Acme/API",
        Some("Services/package-lock.json"),
        "vite",
        "active",
    );
    task.description.push_str(
        "\n\nCovered by existing open PR #42. manifest=services/package-lock.json; pkg=other;",
    );
    assert!(description_matches_remediation(
        &task.description,
        "acme/api",
        Some("Services/package-lock.json"),
        "vite"
    ));
    assert!(!description_matches_remediation(
        &task.description,
        "acme/api",
        Some("services/package-lock.json"),
        "vite"
    ));
    assert!(!description_matches_remediation(
        &task.description,
        "acme/api",
        Some("Services/package-lock.json"),
        "other"
    ));
    task.description.push_str(SOURCE_MARKER);
    task.description.push_str("{}");
    assert!(stored_identity(&task.description).is_none());
}

#[test]
fn admission_reuses_proven_legacy_work_without_spending_queue_capacity() {
    let original = legacy(
        "acme/api",
        Some("Services/package-lock.json"),
        "vite",
        "active",
    );
    let before = snapshot(&original);
    let mut known = BTreeMap::from([(
        original.id.clone(),
        (original.status.clone(), original.description.clone()),
    )]);
    let mut admitted = Vec::new();
    let mut slots = 0;
    let distinct = discovered("acme/api", Some("services/package-lock.json"), "vite", 18);
    assert!(!append_bounded_tasks(
        &mut admitted,
        &mut known,
        vec![
            discovered("acme/api", Some("Services/package-lock.json"), "vite", 99),
            distinct.clone(),
        ],
        &mut slots,
        1
    ));
    assert_eq!(slots, 1);
    assert_eq!(admitted.len(), 1);
    assert_eq!(admitted[0].id, distinct.id);
    let (merged, queued) = merge_discovered_tasks(vec![original], admitted);
    assert_eq!(queued, 1);
    assert_eq!(snapshot(&merged[0]), before);
}

#[test]
fn ambiguity_is_reported_even_if_versioned_work_already_exists() {
    let mut original = legacy("acme/api", Some("package-lock.json"), "vite", "done");
    original.description.clear();
    let mut candidate = discovered("acme/api", Some("package-lock.json"), "vite", 99);
    let known = BTreeMap::from([
        (original.id.clone(), original.description.clone()),
        (candidate.id.clone(), candidate.description.clone()),
    ]);
    for _ in 0..2 {
        let (matching_id, warning) =
            match_remediation_task(&mut candidate, |id| known.get(id).map(String::as_str));
        assert_eq!(matching_id, candidate.id);
        assert!(warning.unwrap().contains(&original.id));
    }
    assert_eq!(
        candidate
            .description
            .matches("Remediation identity ambiguity:")
            .count(),
        1
    );
}

#[test]
fn existing_v2_and_legacy_history_are_not_renamed_or_combined() {
    let original = legacy("acme/api", Some("package-lock.json"), "vite", "done");
    let mut versioned = discovered("acme/api", Some("package-lock.json"), "vite", 99);
    versioned.status = "active".into();
    versioned.run = Some("versioned-run".into());
    let before = vec![snapshot(&original), snapshot(&versioned)];
    let (merged, queued) =
        merge_discovered_tasks(vec![original, versioned.clone()], vec![versioned]);
    assert_eq!(queued, 0);
    assert_eq!(merged.iter().map(snapshot).collect::<Vec<_>>(), before);
}

#[test]
fn merge_rechecks_legacy_metadata_in_the_latest_persisted_state() {
    let original = legacy(
        "acme/api",
        Some("Services/package-lock.json"),
        "vite",
        "done",
    );
    let mut candidate = discovered("acme/api", Some("Services/package-lock.json"), "vite", 99);
    let candidate_id = candidate.id.clone();
    let (matching_id, warning) = match_remediation_task(&mut candidate, |id| {
        (id == original.id).then_some(original.description.as_str())
    });
    assert_eq!(matching_id, original.id);
    assert!(warning.is_none());
    assert_eq!(candidate.id, candidate_id);
    let different = legacy(
        "acme/api",
        Some("services/package-lock.json"),
        "vite",
        "done",
    );
    assert_eq!(different.id, original.id);
    let before = snapshot(&different);
    let (merged, queued) = merge_discovered_tasks(vec![different], vec![candidate]);
    assert_eq!(queued, 1);
    assert_eq!(snapshot(&merged[0]), before);
    assert_eq!(merged[1].id, candidate_id);
}
