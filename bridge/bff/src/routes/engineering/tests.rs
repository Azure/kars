// kars Bridge BFF — engineering intake regression tests.
use k8s_openapi::api::core::v1::ConfigMap;

use super::*;

pub(super) fn pull(login: &str, head: &str, number: u64) -> GithubPull {
    GithubPull {
        number,
        html_url: format!("https://github.com/acme/api/pull/{number}"),
        title: "Bump serde from 1.0.1 to 1.0.2".into(),
        draft: false,
        updated_at: "2026-07-20T12:00:00Z".into(),
        user: Some(GithubUser {
            login: login.into(),
        }),
        base: GithubRef {
            name: "main".into(),
        },
        head: GithubHead {
            name: head.into(),
            sha: "abc123".into(),
        },
        labels: vec![GithubLabel {
            name: "dependencies".into(),
        }],
    }
}

fn task(id: &str, status: &str) -> TeamTaskDto {
    TeamTaskDto {
        id: id.into(),
        title: id.into(),
        description: String::new(),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: false,
        status: status.into(),
        run: (status == "active").then(|| "run-1".into()),
        created_at: Some("2026-07-20T00:00:00Z".into()),
        done_at: (status == "done").then(|| "2026-07-20T01:00:00Z".into()),
        stuck_since: (status == "active").then(|| "2026-07-20T00:30:00Z".into()),
        assignment_nonce: None,
    }
}

#[test]
fn deterministic_ids_are_repo_and_pr_scoped() {
    assert_eq!(work_id("Acme/API", 42), work_id("acme/api", 42));
    assert_ne!(work_id("acme/api", 42), work_id("acme/api", 43));
    assert_ne!(work_id("acme/api", 42), work_id("acme/web", 42));
    assert_eq!(source_id("Acme/API", 42), "github:acme/api:pull:42");
    assert_eq!(
        source_config_map_name("kars-system", "platform"),
        source_config_map_name("kars-system", "platform")
    );
    assert_ne!(
        source_config_map_name("tenant-a", "platform"),
        source_config_map_name("tenant-b", "platform")
    );
    assert_eq!(
        remediation_work_id("Acme/API", Some("package-lock.json"), "@babel/core"),
        remediation_work_id("acme/api", Some("package-lock.json"), "@babel/core")
    );
    assert_ne!(
        remediation_work_id("acme/api", Some("package-lock.json"), "@babel/core"),
        remediation_work_id("acme/api", Some("other/package-lock.json"), "@babel/core")
    );
}

#[test]
fn alert_tasks_lead_with_authoritative_source_facts() {
    let task = alert_backlog_task(
        EngineeringSignal::DependabotAlert,
        GithubAlertRef {
            repo: "pallakatos/kars",
            number: 9,
        },
        "vite alert".into(),
        "Dependabot reported a finding.",
        serde_json::json!({
            "manifest_path": "tests/compat/package-lock.json",
            "package": "vite",
            "vulnerable_version_range": ">= 8.0.0, <= 8.0.15",
            "first_patched_version": "8.0.16",
            "ghsa_id": "GHSA-v6wh-96g9-6wx3",
        }),
        None,
        "2026-07-23T00:00:00Z",
    );
    let prefix = task.description.lines().next().unwrap_or_default();
    assert!(prefix.contains("manifest=tests/compat/package-lock.json"));
    assert!(prefix.contains("fixed=8.0.16"));
    assert!(prefix.contains("ghsa=GHSA-v6wh-96g9-6wx3"));
    assert!(prefix.contains("max2 same target"));
    assert!(prefix.contains("search open+merged PRs"));
    assert!(prefix.contains("no principal substitution"));
}

#[test]
fn legacy_remediation_matching_is_exact_and_repo_scoped() {
    let description = concat!(
        "AUTH SOURCE: manifest=package-lock.json; pkg=react-dom; ghsa=GHSA-a. ",
        "\n\nStructured source details (JSON):\n",
        "{\"signal\":\"dependabot_alert\",\"work_id\":\"dependabot-alert-legacy\",",
        "\"repo\":\"acme/web\",\"details\":{\"manifest_path\":\"package-lock.json\",",
        "\"package\":\"react-dom\"}}"
    );
    assert!(description_matches_remediation(
        description,
        "acme/web",
        Some("package-lock.json"),
        "react-dom",
    ));
    assert!(!description_matches_remediation(
        description,
        "acme/api",
        Some("package-lock.json"),
        "react-dom",
    ));
    assert!(!description_matches_remediation(
        description,
        "acme/web",
        Some("package-lock.json"),
        "react",
    ));
    assert!(!description_matches_remediation(
        description,
        "acme/web",
        None,
        "react-dom",
    ));
}

#[test]
fn dependabot_detection_accepts_bot_login_or_head_prefix() {
    assert!(is_dependabot_pr(&pull(
        "dependabot[bot]",
        "renovate/foo",
        1
    )));
    assert!(is_dependabot_pr(&pull(
        "someone",
        "dependabot/npm/foo-1.2.3",
        2
    )));
    assert!(!is_dependabot_pr(&pull("renovate[bot]", "renovate/foo", 3)));
}

#[test]
fn open_pull_candidates_mention_the_same_package_or_advisory() {
    let alert = GithubDependabotAlert {
        number: 17,
        html_url: "https://github.com/acme/api/security/dependabot/17".into(),
        dependency: GithubDependabotDependency {
            package: GithubPackage {
                ecosystem: "npm".into(),
                name: "@babel/core".into(),
            },
            manifest_path: Some("package-lock.json".into()),
            scope: Some("development".into()),
        },
        security_advisory: Some(GithubSecurityAdvisory {
            ghsa_id: "GHSA-aaaa-bbbb-cccc".into(),
            cve_id: None,
            summary: "test".into(),
            severity: "high".into(),
        }),
        security_vulnerability: GithubSecurityVulnerability {
            vulnerable_version_range: "< 8".into(),
            first_patched_version: Some(GithubPatchedVersion {
                identifier: "8.0.0".into(),
            }),
        },
        updated_at: None,
    };
    let mut package_pr = pull("agent", "fix-babel-core", 42);
    package_pr.title = "chore: bump @babel/core to 8.0.0".into();
    assert!(open_pull_may_address_dependabot_alert(&package_pr, &alert));
    let mut advisory_pr = pull("agent", "security-fix", 43);
    advisory_pr.title = "fix GHSA-aaaa-bbbb-cccc".into();
    assert!(open_pull_may_address_dependabot_alert(&advisory_pr, &alert));
    let unrelated = pull("agent", "fix-vite", 44);
    assert!(!open_pull_may_address_dependabot_alert(&unrelated, &alert));
}

#[test]
fn parses_github_pull_and_builds_structured_task() {
    let raw = serde_json::json!({
        "number": 7,
        "html_url": "https://github.com/acme/api/pull/7",
        "title": "Bump axum",
        "draft": true,
        "updated_at": "2026-07-20T12:00:00Z",
        "user": {"login": "dependabot[bot]"},
        "base": {"ref": "main"},
        "head": {"ref": "dependabot/cargo/axum-1", "sha": "deadbeef"},
        "labels": [{"name": "dependencies"}, {"name": "rust"}]
    });
    let parsed: GithubPull = serde_json::from_value(raw).unwrap();
    let task = backlog_task("acme/api", &parsed, "2026-07-20T13:00:00Z");
    assert_eq!(task.status, "pending");
    assert!(task.description.contains("\"head_sha\":\"deadbeef\""));
    assert!(task.description.contains("\"draft\":true"));
    assert!(task.description.contains("Never claim CI is green"));
}

#[test]
fn security_alert_ids_are_stable_and_signal_scoped() {
    assert_eq!(
        alert_work_id(EngineeringSignal::CodeScanningAlert, "Acme/API", 42),
        alert_work_id(EngineeringSignal::CodeScanningAlert, "acme/api", 42)
    );
    assert_ne!(
        alert_work_id(EngineeringSignal::CodeScanningAlert, "acme/api", 42),
        alert_work_id(EngineeringSignal::DependabotAlert, "acme/api", 42)
    );
    assert_eq!(
        alert_source_id(EngineeringSignal::SecretScanningAlert, "Acme/API", 7),
        "github:acme/api:secret-scanning-alert:7"
    );
}

#[test]
fn code_scanning_alert_builds_actionable_task() {
    let alert: GithubCodeScanningAlert = serde_json::from_value(serde_json::json!({
        "number": 12,
        "html_url": "https://github.com/acme/api/security/code-scanning/12",
        "rule": {
            "id": "rust/path-injection",
            "name": "Path injection",
            "description": "User-controlled path reaches filesystem access",
            "severity": "error",
            "security_severity_level": "high"
        },
        "most_recent_instance": {
            "location": {"path": "src/files.rs", "start_line": 44, "end_line": 47}
        },
        "updated_at": "2026-07-21T00:00:00Z"
    }))
    .unwrap();
    let task = code_scanning_task("acme/api", &alert, "2026-07-21T01:00:00Z");
    assert!(task.title.contains("Path injection"));
    assert!(task.description.contains("\"severity\":\"high\""));
    assert!(task.description.contains("\"path\":\"src/files.rs\""));
    assert!(task.description.contains("Never claim success"));
}

#[test]
fn secret_scanning_task_never_persists_secret_value() {
    let secret = "ghp_live_secret_value";
    let alert: GithubSecretScanningAlert = serde_json::from_value(serde_json::json!({
        "number": 9,
        "html_url": "https://github.com/acme/api/security/secret-scanning/9",
        "secret_type": "github_personal_access_token",
        "secret_type_display_name": "GitHub Personal Access Token",
        "secret": secret,
        "resolution": null,
        "created_at": "2026-07-21T00:00:00Z",
        "updated_at": "2026-07-21T00:00:00Z"
    }))
    .unwrap();
    let task = secret_scanning_task("acme/api", &alert, "2026-07-21T01:00:00Z");
    assert!(task.description.contains("do not print, persist, or copy"));
    assert!(!task.description.contains(secret));
}

#[test]
fn private_repo_without_security_products_is_unavailable_not_error() {
    let features = GithubRepositoryFeatures {
        private: true,
        security_and_analysis: None,
    };
    let code_error = GithubListError {
        state: EngineeringSignalSyncState::Forbidden,
        detail: "HTTP 403".into(),
    };
    let secret_error = GithubListError {
        state: EngineeringSignalSyncState::Unavailable,
        detail: "HTTP 404".into(),
    };
    for (signal, error) in [
        (EngineeringSignal::CodeScanningAlert, code_error),
        (EngineeringSignal::SecretScanningAlert, secret_error),
    ] {
        let mapped = unavailable_security_product(Some(&features), signal, &error).unwrap();
        assert_eq!(mapped.state, EngineeringSignalSyncState::Unavailable);
        assert!(mapped.detail.contains("not enabled or licensed"));
    }
}

#[test]
fn github_link_parser_finds_next_page() {
    assert_eq!(
            next_link(
                r#"<https://api.github.com/repositories/1/alerts?page=2>; rel="next", <https://api.github.com/repositories/1/alerts?page=4>; rel="last""#
            )
            .as_deref(),
            Some("https://api.github.com/repositories/1/alerts?page=2")
        );
    assert_eq!(next_link(""), None);
}

#[test]
fn dedupe_preserves_existing_active_and_done_tasks() {
    let mut active = task("dependabot-pr-active", "active");
    active.assignment_nonce = Some("run-1-assign-7".into());
    let done = task("dependabot-pr-done", "done");
    let (merged, added) = merge_discovered_tasks(
        vec![active.clone(), done.clone()],
        vec![
            task("dependabot-pr-active", "pending"),
            task("dependabot-pr-done", "pending"),
            task("dependabot-pr-new", "pending"),
        ],
    );
    assert_eq!(added, 1);
    assert_eq!(merged.len(), 3);
    assert_eq!(merged[0].status, "active");
    assert_eq!(merged[0].run, active.run);
    assert_eq!(merged[0].assignment_nonce, active.assignment_nonce);
    assert!(merged[0].review_required);
    assert_eq!(merged[1].status, "done");
    assert_eq!(merged[1].done_at, done.done_at);
    assert!(merged[1].review_required);
}

#[test]
fn changed_open_security_alert_requeues_completed_work() {
    let mut completed = task("code-scanning-alert-abc", "done");
    completed.description = "updated_at=old".into();
    let mut rediscovered = task("code-scanning-alert-abc", "pending");
    rediscovered.description = "updated_at=new".into();
    let (merged, queued) = merge_discovered_tasks(vec![completed], vec![rediscovered.clone()]);
    assert_eq!(queued, 1);
    assert_eq!(merged[0].status, "pending");
    assert_eq!(merged[0].description, rediscovered.description);
    assert!(merged[0].run.is_none());
    assert!(merged[0].done_at.is_none());

    let (unchanged, queued) = merge_discovered_tasks(merged, vec![rediscovered]);
    assert_eq!(queued, 0);
    assert_eq!(unchanged[0].status, "pending");

    let mut refreshed = task("code-scanning-alert-abc", "pending");
    refreshed.description = "updated_at=newer".into();
    let (refreshed_tasks, queued) = merge_discovered_tasks(unchanged, vec![refreshed.clone()]);
    assert_eq!(queued, 0);
    assert_eq!(refreshed_tasks[0].description, refreshed.description);
}

#[test]
fn changed_pending_alert_flows_through_without_using_queue_capacity() {
    let mut existing = task("secret-scanning-alert-abc", "pending");
    existing.description = "updated_at=old".into();
    let mut refreshed = task("secret-scanning-alert-abc", "pending");
    refreshed.description = "updated_at=new".into();
    let mut candidates = Vec::new();
    let mut known = BTreeMap::from([(
        existing.id.clone(),
        (existing.status.clone(), existing.description.clone()),
    )]);
    let mut queued_slots = 0;
    assert!(!append_bounded_tasks(
        &mut candidates,
        &mut known,
        vec![refreshed.clone()],
        &mut queued_slots,
        1,
    ));
    assert_eq!(queued_slots, 0);
    assert_eq!(candidates.len(), 1);
    let (merged, queued) = merge_discovered_tasks(vec![existing], candidates);
    assert_eq!(queued, 0);
    assert_eq!(merged[0].description, refreshed.description);
}

#[test]
fn changed_active_alert_refreshes_source_facts_without_restarting_run() {
    let mut existing = task("dependabot-alert-abc", "active");
    existing.description = "old source facts".into();
    existing.run = Some("run-in-progress".into());
    let mut refreshed = task("dependabot-alert-abc", "pending");
    refreshed.description =
        "AUTHORITATIVE SOURCE FACTS: manifest_path=tests/compat/package-lock.json".into();
    let (merged, queued) = merge_discovered_tasks(vec![existing], vec![refreshed.clone()]);
    assert_eq!(queued, 0);
    assert_eq!(merged[0].status, "active");
    assert_eq!(merged[0].run.as_deref(), Some("run-in-progress"));
    assert_eq!(merged[0].description, refreshed.description);
}

#[test]
fn covered_pending_alert_is_retired_without_touching_active_run() {
    let mut pending = task("dependabot-alert-pending", "pending");
    let mut retirement = task("dependabot-alert-pending", "done");
    retirement.description = "Covered by existing open PR #42".into();
    retirement.done_at = Some("2026-07-23T00:00:00Z".into());
    let (merged, queued) = merge_discovered_tasks(vec![pending.clone()], vec![retirement]);
    assert_eq!(queued, 0);
    assert_eq!(merged[0].status, "done");
    assert!(merged[0].run.is_none());

    pending.status = "active".into();
    pending.run = Some("run-in-progress".into());
    let mut covered = task("dependabot-alert-pending", "done");
    covered.description = "Covered by existing open PR #42".into();
    let (active, queued) = merge_discovered_tasks(vec![pending], vec![covered]);
    assert_eq!(queued, 0);
    assert_eq!(active[0].status, "active");
    assert_eq!(active[0].run.as_deref(), Some("run-in-progress"));
}

#[test]
fn repeated_human_review_decision_requeues_completed_task() {
    let completed = task("github-pr-merge-abc", "done");
    let decision = task("github-pr-merge-abc", "pending");
    let (merged, queued) = merge_discovered_tasks(vec![completed], vec![decision]);
    assert_eq!(queued, 1);
    assert_eq!(merged[0].status, "pending");
    assert!(merged[0].run.is_none());
    assert!(merged[0].done_at.is_none());
}

#[test]
fn repo_authorization_and_limits_are_enforced() {
    let granted = (0..=MAX_REPOS)
        .map(|i| format!("acme/repo-{i}"))
        .collect::<Vec<_>>();
    let too_many = PutEngineeringSourceRequest {
        enabled: true,
        auto_run: true,
        repos: granted.clone(),
        signals: vec![EngineeringSignal::DependabotPr],
        poll_interval_seconds: DEFAULT_POLL_INTERVAL_SECONDS,
    };
    assert!(validate_request(&too_many, &granted).is_err());

    let unauthorized = PutEngineeringSourceRequest {
        enabled: true,
        auto_run: true,
        repos: vec!["other/private".into()],
        signals: vec![EngineeringSignal::DependabotPr],
        poll_interval_seconds: DEFAULT_POLL_INTERVAL_SECONDS,
    };
    assert!(validate_request(&unauthorized, &granted).is_err());

    let invalid_interval = PutEngineeringSourceRequest {
        enabled: true,
        auto_run: true,
        repos: vec!["acme/repo-0".into()],
        signals: vec![EngineeringSignal::DependabotPr],
        poll_interval_seconds: MIN_POLL_INTERVAL_SECONDS - 1,
    };
    assert!(validate_request(&invalid_interval, &granted).is_err());
}

#[test]
fn config_cursor_and_status_serialize_round_trip() {
    let config = EngineeringSourceConfig {
        version: 1,
        team_namespace: "kars-system".into(),
        team_name: "platform".into(),
        owner_sub: "subject-1".into(),
        connection_config_map_ref: "kars-github-connection-deadbeef".into(),
        enabled: true,
        auto_run: true,
        repos: vec!["acme/api".into()],
        signals: vec![EngineeringSignal::DependabotPr],
        poll_interval_seconds: 900,
    };
    let cursor = EngineeringCursor {
        repository_updated_at: BTreeMap::from([("acme/api".into(), "2026-07-20T12:00:00Z".into())]),
    };
    let status = EngineeringSourceStatus {
        state: EngineeringSyncState::Ok,
        last_sync_at: Some("2026-07-20T12:00:00Z".into()),
        last_success_at: Some("2026-07-20T12:00:00Z".into()),
        last_error: None,
        items_discovered: 2,
        items_queued: 1,
        total_items_queued: 4,
        next_poll_at: Some("2026-07-20T12:15:00Z".into()),
        ..Default::default()
    };
    let data = source_data(&config, &cursor, &status).unwrap();
    let cm = ConfigMap {
        data: Some(data),
        ..Default::default()
    };
    let round_trip = parse_source(&cm).unwrap();
    assert_eq!(round_trip, (config, cursor, status));
}

fn clean_pull_status() -> serde_json::Value {
    serde_json::json!({
        "state": "open",
        "merged": false,
        "draft": false,
        "mergeable": true,
        "mergeable_state": "clean"
    })
}

#[test]
fn review_readiness_only_flags_green_clean_prs() {
    let (state, _, total, passed) = classify_review_readiness(
        &clean_pull_status(),
        &serde_json::json!({
            "check_runs": [
                {"status":"completed","conclusion":"success"},
                {"status":"completed","conclusion":"neutral"}
            ]
        }),
        &serde_json::json!({"state":"success","statuses":[]}),
    );
    assert_eq!(state, EngineeringReviewState::ReadyForReview);
    assert_eq!((total, passed), (2, 2));

    let (state, _, _, _) = classify_review_readiness(
        &clean_pull_status(),
        &serde_json::json!({
            "check_runs": [{"status":"in_progress","conclusion":null}]
        }),
        &serde_json::json!({"state":"pending","statuses":[]}),
    );
    assert_eq!(state, EngineeringReviewState::WaitingForCi);

    let (state, _, _, _) = classify_review_readiness(
        &clean_pull_status(),
        &serde_json::json!({
            "check_runs": [{"status":"completed","conclusion":"failure"}]
        }),
        &serde_json::json!({"state":"failure","statuses":[]}),
    );
    assert_eq!(state, EngineeringReviewState::CiFailed);

    let (state, _, _, _) = classify_review_readiness(
        &clean_pull_status(),
        &serde_json::json!({
            "total_count": 101,
            "check_runs": (0..100).map(|_| serde_json::json!({
                "status":"completed","conclusion":"success"
            })).collect::<Vec<_>>()
        }),
        &serde_json::json!({"state":"success","statuses":[]}),
    );
    assert_eq!(state, EngineeringReviewState::WaitingForCi);
}

#[test]
fn red_pr_creates_deterministic_followup_work() {
    let item = EngineeringReviewItem {
        repo: "acme/api".into(),
        pr_number: 42,
        pr_url: "https://github.com/acme/api/pull/42".into(),
        title: "Fix dependency".into(),
        run: "run-1".into(),
        source_id: "github:acme/api:pull:42".into(),
        work_id: "dependabot-pr-example".into(),
        task_status: "done".into(),
        run_state: Some("Completed".into()),
        selected_roles: vec!["reviewer".into()],
        delivered_roles: vec!["reviewer".into()],
        artifact_count: Some(1),
        head_sha: "abc123".into(),
        state: EngineeringReviewState::CiFailed,
        detail: "test failed".into(),
        checks_total: 2,
        checks_passed: 1,
        observed_at: "2026-07-20T12:00:00Z".into(),
    };
    let first = review_followup_task(&item, "2026-07-20T12:00:00Z").unwrap();
    let second = review_followup_task(&item, "2026-07-20T13:00:00Z").unwrap();
    assert_eq!(first.id, second.id);
    let mut changed_head = item.clone();
    changed_head.head_sha = "def456".into();
    let changed = review_followup_task(&changed_head, "2026-07-20T14:00:00Z").unwrap();
    assert_eq!(first.id, changed.id);
    assert_ne!(first.description, changed.description);
    assert!(first.description.contains("Never claim green"));
    assert!(first.description.contains("superseded"));
    assert!(first.description.contains("do not repair or rebase it"));
}

#[test]
fn duplicate_prs_create_one_canonical_retirement_task() {
    let first = pull("agent", "fix-js-yaml", 18);
    let second = pull("agent", "fix-js-yaml-again", 24);
    let task = dedupe_followup_task(
        "acme/api",
        "dependency-remediation-abc",
        &[&second, &first],
        "2026-07-20T12:00:00Z",
    )
    .unwrap();
    assert!(task.title.contains("PR #18"));
    assert!(task.description.contains("#24"));
    assert!(task.description.contains("never merge"));
}
