// kars Bridge BFF — review helpers for engineering intake.

use std::collections::{BTreeSet, HashSet};

use crate::kars::cluster::Cluster;
use crate::providers::signing::sha256;
use crate::routes::teams::read_task_list;

use super::github::github_get_json;
use super::intake::source_id;
use super::{
    EngineeringReviewItem, EngineeringReviewState, EngineeringSourceConfig, GithubPull,
    MAX_REVIEW_PRS_PER_SYNC, ReviewExecution, TeamTaskDto,
};

pub(super) fn classify_review_readiness(
    pull: &serde_json::Value,
    check_runs: &serde_json::Value,
    status: &serde_json::Value,
) -> (EngineeringReviewState, String, usize, usize) {
    let runs = check_runs
        .get("check_runs")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let statuses = status
        .get("statuses")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let total_count = check_runs
        .get("total_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(runs.len() as u64) as usize;
    let total = total_count + statuses.len();
    let passed_runs = runs
        .iter()
        .filter(|run| {
            run.get("status").and_then(serde_json::Value::as_str) == Some("completed")
                && matches!(
                    run.get("conclusion").and_then(serde_json::Value::as_str),
                    Some("success" | "neutral" | "skipped")
                )
        })
        .count();
    let passed_statuses = statuses
        .iter()
        .filter(|item| item.get("state").and_then(serde_json::Value::as_str) == Some("success"))
        .count();
    let passed = passed_runs + passed_statuses;

    if pull.get("draft").and_then(serde_json::Value::as_bool) == Some(true) {
        return (
            EngineeringReviewState::Blocked,
            "PR is still a draft.".into(),
            total,
            passed,
        );
    }
    if pull.get("mergeable").and_then(serde_json::Value::as_bool) == Some(false) {
        return (
            EngineeringReviewState::Blocked,
            "GitHub reports merge conflicts.".into(),
            total,
            passed,
        );
    }
    let mergeable_state = pull
        .get("mergeable_state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    if matches!(mergeable_state, "dirty" | "blocked" | "behind") {
        return (
            EngineeringReviewState::Blocked,
            format!("Branch state is '{mergeable_state}', not clean and up to date."),
            total,
            passed,
        );
    }
    let combined_status = status
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("pending");
    if runs.iter().any(|run| {
        run.get("status").and_then(serde_json::Value::as_str) == Some("completed")
            && !matches!(
                run.get("conclusion").and_then(serde_json::Value::as_str),
                Some("success" | "neutral" | "skipped")
            )
    }) || matches!(combined_status, "failure" | "error")
    {
        return (
            EngineeringReviewState::CiFailed,
            format!("{passed}/{total} GitHub checks passed; at least one check is red."),
            total,
            passed,
        );
    }
    if total == 0 {
        return (
            EngineeringReviewState::WaitingForCi,
            "No GitHub CI/status evidence exists for the head commit yet.".into(),
            total,
            passed,
        );
    }
    if total_count > runs.len()
        || passed < total
        || runs
            .iter()
            .any(|run| run.get("status").and_then(serde_json::Value::as_str) != Some("completed"))
        || (!statuses.is_empty() && combined_status != "success")
        || pull.get("mergeable").and_then(serde_json::Value::as_bool) != Some(true)
        || mergeable_state != "clean"
    {
        return (
            EngineeringReviewState::WaitingForCi,
            format!("{passed}/{total} GitHub checks passed; waiting for a clean mergeable state."),
            total,
            passed,
        );
    }
    (
        EngineeringReviewState::ReadyForReview,
        format!("GitHub reports a clean, up-to-date PR with {passed}/{total} checks green."),
        total,
        passed,
    )
}

async fn inspect_review_item(
    client: &reqwest::Client,
    token: &str,
    repo: &str,
    number: u64,
    execution: ReviewExecution<'_>,
    observed_at: &str,
) -> Result<Option<EngineeringReviewItem>, String> {
    let ReviewExecution {
        run,
        work_id,
        task_status,
        run_state,
        selected_roles,
        delivered_roles,
        artifact_count,
    } = execution;
    let pull = github_get_json(
        client,
        token,
        &format!("https://api.github.com/repos/{repo}/pulls/{number}"),
    )
    .await?;
    if pull.get("state").and_then(serde_json::Value::as_str) != Some("open")
        || pull.get("merged").and_then(serde_json::Value::as_bool) == Some(true)
    {
        return Ok(None);
    }
    let head_sha = pull
        .get("head")
        .and_then(|head| head.get("sha"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("GitHub PR {repo}#{number} has no head SHA"))?
        .to_string();
    let check_runs = github_get_json(
        client,
        token,
        &format!("https://api.github.com/repos/{repo}/commits/{head_sha}/check-runs?per_page=100"),
    )
    .await?;
    let status = github_get_json(
        client,
        token,
        &format!("https://api.github.com/repos/{repo}/commits/{head_sha}/status?per_page=100"),
    )
    .await?;
    let (state, detail, checks_total, checks_passed) =
        classify_review_readiness(&pull, &check_runs, &status);
    Ok(Some(EngineeringReviewItem {
        repo: repo.to_string(),
        pr_number: number,
        pr_url: pull
            .get("html_url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        title: pull
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Pull request")
            .to_string(),
        run: run.to_string(),
        source_id: source_id(repo, number),
        work_id: work_id.to_string(),
        task_status: task_status.to_string(),
        run_state,
        selected_roles,
        delivered_roles,
        artifact_count,
        head_sha,
        state,
        detail,
        checks_total,
        checks_passed,
        observed_at: observed_at.to_string(),
    }))
}

async fn run_execution_summary(
    cluster: &Cluster,
    namespace: &str,
    run: &str,
) -> (Option<String>, Vec<String>, Vec<String>) {
    let task = cluster.tasks(namespace).get_opt(run).await.ok().flatten();
    let mut selected = BTreeSet::new();
    let mut delivered = BTreeSet::new();
    let run_state = task
        .as_ref()
        .and_then(|task| task.status.as_ref())
        .and_then(|status| status.assignment.as_ref())
        .map(|assignment| assignment.state.clone());
    if let Some(events) = task
        .as_ref()
        .and_then(|task| task.status.as_ref())
        .map(|status| status.assignment_events.as_slice())
    {
        for event in events {
            let Some(role) = event.child_role.as_ref() else {
                continue;
            };
            selected.insert(role.clone());
            if event.stage.as_deref() == Some("child_handback")
                && event.outcome.as_deref() == Some("success")
                && event.state == "Completed"
            {
                delivered.insert(role.clone());
            }
        }
    }
    (
        run_state,
        selected.into_iter().collect(),
        delivered.into_iter().collect(),
    )
}

pub(super) fn review_followup_task(
    item: &EngineeringReviewItem,
    created_at: &str,
) -> Option<TeamTaskDto> {
    if !matches!(
        item.state,
        EngineeringReviewState::CiFailed | EngineeringReviewState::Blocked
    ) {
        return None;
    }
    let digest = sha256(format!("github-review:{}:{}", item.repo, item.pr_number).as_bytes());
    Some(TeamTaskDto {
        id: format!("github-pr-fix-{}", hex::encode(&digest[..10])),
        title: format!(
            "[PR gate] Resolve or retire {} PR #{} before review",
            item.repo, item.pr_number
        ),
        description: format!(
            "GitHub does not consider this PR ready for human review. Before modifying the branch, determine whether the PR is still needed or has been superseded by a merged PR/default-branch change. If it is superseded, do not repair or rebase it: close it when authorized, or report the exact closure recommendation. Only when its objective is still required should you resolve the observed branch/CI state, push the smallest correction, and wait for exact-SHA GitHub checks. Never claim green from local inference and never merge.\n\nPR: {}\nHead SHA: {}\nObserved state: {:?}\nDetail: {}",
            item.pr_url, item.head_sha, item.state, item.detail
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: true,
        status: "pending".into(),
        run: None,
        created_at: Some(created_at.to_string()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    })
}

pub(super) fn dedupe_followup_task(
    repo: &str,
    remediation_id: &str,
    pulls: &[&GithubPull],
    created_at: &str,
) -> Option<TeamTaskDto> {
    if pulls.len() < 2 {
        return None;
    }
    let mut ordered = pulls.to_vec();
    ordered.sort_by_key(|pull| pull.number);
    let canonical = ordered[0];
    let duplicates = ordered[1..]
        .iter()
        .map(|pull| format!("#{} {}", pull.number, pull.html_url))
        .collect::<Vec<_>>()
        .join(", ");
    let digest = sha256(format!("{repo}:{remediation_id}").as_bytes());
    Some(TeamTaskDto {
        id: format!("github-pr-dedupe-{}", hex::encode(&digest[..10])),
        title: format!(
            "[PR dedupe] Review {repo} PR #{} and {} possible duplicate(s)",
            canonical.number,
            ordered.len() - 1
        ),
        description: format!(
            "Multiple open pull requests mention this remediation's package or advisory. Their titles are not coverage evidence. Compare actual changed files with the exact case-sensitive manifest, package, advisory and head-SHA checks before treating any work as equivalent. Preserve distinct manifest fixes. Only after equivalence is verified, preserve the oldest canonical PR unless a newer PR has strictly better, already-green evidence and close superseded duplicates; never merge. Report exact URLs/head SHAs/check states.\n\nCanonical candidate: #{} {}\nDuplicate candidates: {}",
            canonical.number, canonical.html_url, duplicates
        ),
        depends_on: Vec::new(),
        acceptance_criteria: Vec::new(),
        review_required: true,
        status: "pending".into(),
        run: None,
        created_at: Some(created_at.to_string()),
        done_at: None,
        stuck_since: None,
        assignment_nonce: None,
    })
}

pub(super) async fn collect_review_items(
    cluster: &Cluster,
    client: &reqwest::Client,
    token: &str,
    config: &EngineeringSourceConfig,
    observed_at: &str,
) -> (Vec<EngineeringReviewItem>, Vec<TeamTaskDto>, Vec<String>) {
    let backlog = read_task_list(&cluster.read_team_tasks(&config.team_name).await);
    let configured_repos = config
        .repos
        .iter()
        .map(|repo| repo.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut followups = Vec::new();
    let mut errors = Vec::new();
    for task in backlog.iter().rev() {
        if seen.len() >= MAX_REVIEW_PRS_PER_SYNC {
            errors.push(format!(
                "review readiness reached the {MAX_REVIEW_PRS_PER_SYNC}-PR sync cap"
            ));
            break;
        }
        let Some(run) = task.run.as_deref() else {
            continue;
        };
        let Some(output) = cluster.read_mission_output(run).await else {
            continue;
        };
        let (run_state, selected_roles, delivered_roles) =
            run_execution_summary(cluster, &config.team_namespace, run).await;
        let artifact_count = output
            .get("artifactCount")
            .and_then(|value| value.parse::<usize>().ok());
        let text = output.get("output").map(String::as_str).unwrap_or_default();
        if !crate::routes::tasks::is_real_deliverable(
            output.get("status").map(String::as_str),
            text,
        ) {
            continue;
        }
        for pull in crate::routes::tasks::extract_pull_requests(text) {
            let key = format!("{}#{}", pull.repo.to_ascii_lowercase(), pull.number);
            if !configured_repos.contains(&pull.repo.to_ascii_lowercase()) || !seen.insert(key) {
                continue;
            }
            match inspect_review_item(
                client,
                token,
                &pull.repo,
                pull.number as u64,
                ReviewExecution {
                    run,
                    work_id: &task.id,
                    task_status: &task.status,
                    run_state: run_state.clone(),
                    selected_roles: selected_roles.clone(),
                    delivered_roles: delivered_roles.clone(),
                    artifact_count,
                },
                observed_at,
            )
            .await
            {
                Ok(Some(item)) => {
                    if let Some(task) = review_followup_task(&item, observed_at) {
                        followups.push(task);
                    }
                    items.push(item);
                }
                Ok(None) => {}
                Err(error) => errors.push(format!(
                    "review readiness for {}#{} failed: {error}",
                    pull.repo, pull.number
                )),
            }
        }
    }
    (items, followups, errors)
}
