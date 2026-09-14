// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// kars Bridge BFF — remediation identity and compatibility with persisted intake.

use crate::providers::signing::sha256;
use std::collections::{BTreeMap, BTreeSet};

use super::{GithubPull, TeamTaskDto};

const SOURCE_MARKER: &str = "\n\nStructured source details (JSON):\n";

#[derive(Debug, PartialEq, Eq)]
struct RemediationIdentity {
    repo: String,
    manifest_path: Option<String>,
    package: String,
}

impl RemediationIdentity {
    fn new(repo: &str, manifest_path: Option<&str>, package: &str) -> Self {
        Self {
            repo: repo.to_ascii_lowercase(),
            manifest_path: manifest_path.map(str::to_string),
            package: package.to_string(),
        }
    }

    fn work_id(&self) -> String {
        let framed = serde_json::json!([2, self.repo, self.manifest_path, self.package]);
        let digest = sha256(framed.to_string().as_bytes());
        format!("dependency-remediation-v2-{}", hex::encode(&digest[..10]))
    }

    // Only locates potentially related history; this lossy hash is never proof of equivalence.
    fn legacy_work_id(&self) -> String {
        let identity = format!(
            "{}:{}:{}",
            self.repo,
            self.manifest_path
                .as_deref()
                .unwrap_or("unknown")
                .to_ascii_lowercase(),
            self.package.to_ascii_lowercase()
        );
        let digest = sha256(identity.as_bytes());
        format!("dependency-remediation-{}", hex::encode(&digest[..10]))
    }
}

pub(super) fn remediation_work_id(
    repo: &str,
    manifest_path: Option<&str>,
    package: &str,
) -> String {
    RemediationIdentity::new(repo, manifest_path, package).work_id()
}

fn source_json(description: &str) -> Option<serde_json::Value> {
    let (_, source) = description.split_once(SOURCE_MARKER)?;
    if source.contains(SOURCE_MARKER) {
        return None;
    }
    // Coverage notes may follow the original JSON. Read that object, not prose or substrings.
    serde_json::Deserializer::from_str(source)
        .into_iter::<serde_json::Value>()
        .next()?
        .ok()
}

fn replace_source(task: &mut TeamTaskDto, source: serde_json::Value) {
    let (prefix, suffix) = task
        .description
        .split_once(SOURCE_MARKER)
        .expect("validated source");
    let mut stream = serde_json::Deserializer::from_str(suffix).into_iter::<serde_json::Value>();
    stream
        .next()
        .expect("source object")
        .expect("valid source object");
    task.description = format!(
        "{prefix}{SOURCE_MARKER}{source}{}",
        &suffix[stream.byte_offset()..]
    );
}

fn stored_identity(description: &str) -> Option<(String, RemediationIdentity)> {
    let source = source_json(description)?;
    if source.get("signal")?.as_str()? != "dependabot_alert" {
        return None;
    }
    let work_id = source.get("work_id")?.as_str()?;
    let repo = source.get("repo")?.as_str()?;
    let details = source.get("details")?;
    let package = details.get("package")?.as_str()?;
    let manifest_path = match details.get("manifest_path")? {
        serde_json::Value::Null => None,
        serde_json::Value::String(path) => Some(path.as_str()),
        _ => return None,
    };
    if work_id.is_empty() || repo.is_empty() || package.is_empty() {
        return None;
    }
    Some((
        work_id.to_string(),
        RemediationIdentity::new(repo, manifest_path, package),
    ))
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct AlertObservation {
    alert_number: u64,
    details: AlertDetails,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct AlertDetails {
    #[serde(flatten)]
    facts: DependencyFacts,
    url: Option<String>,
    updated_at: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct DependencyFacts {
    package: String,
    manifest_path: Option<String>,
    ecosystem: Option<String>,
    scope: Option<String>,
    ghsa_id: Option<String>,
    cve_id: Option<String>,
    summary: Option<String>,
    severity: Option<String>,
    vulnerable_version_range: String,
    first_patched_version: Option<String>,
}

impl AlertObservation {
    fn evidence_key(&self) -> String {
        // Poll metadata and display URLs are not a new advisory or remediation input.
        serde_json::json!([self.alert_number, self.details.facts]).to_string()
    }
}

fn observations(description: &str) -> Option<Vec<AlertObservation>> {
    let source = source_json(description)?;
    let alerts: Vec<AlertObservation> = match source.get("alerts") {
        Some(alerts) => serde_json::from_value(alerts.clone()).ok(),
        None => serde_json::from_value(source).ok().map(|alert| vec![alert]),
    }?;
    let (_, identity) = stored_identity(description)?;
    alerts
        .iter()
        .all(|alert| {
            alert.details.facts.package == identity.package
                && alert.details.facts.manifest_path == identity.manifest_path
        })
        .then_some(alerts)
}

fn source_suffix(description: &str) -> Option<&str> {
    let (_, suffix) = description.split_once(SOURCE_MARKER)?;
    let mut stream = serde_json::Deserializer::from_str(suffix).into_iter::<serde_json::Value>();
    stream.next()?.ok()?;
    Some(&suffix[stream.byte_offset()..])
}

fn canonical_observations(alerts: Vec<AlertObservation>) -> Vec<AlertObservation> {
    let mut by_number = BTreeMap::<u64, AlertObservation>::new();
    for alert in alerts {
        by_number
            .entry(alert.alert_number)
            .and_modify(|current| {
                let ordering = alert
                    .details
                    .updated_at
                    .cmp(&current.details.updated_at)
                    .then_with(|| {
                        serde_json::json!(alert)
                            .to_string()
                            .cmp(&serde_json::json!(current).to_string())
                    });
                if ordering.is_gt() {
                    *current = alert.clone();
                }
            })
            .or_insert(alert);
    }
    by_number.into_values().collect()
}

fn with_observations(template: &TeamTaskDto, alerts: &[AlertObservation], id: &str) -> TeamTaskDto {
    let (_, identity) =
        stored_identity(&template.description).expect("validated remediation source");
    let first = alerts.first();
    let mut task = super::intake::alert_backlog_task(
        super::EngineeringSignal::DependabotAlert,
        super::GithubAlertRef {
            repo: &identity.repo,
            number: first.map_or(0, |alert| alert.alert_number),
        },
        format!(
            "[Dependabot alerts] {}: {} ({} open findings)",
            identity.repo,
            identity.package,
            alerts.len()
        ),
        "GitHub Dependabot open findings are grouped by exact manifest and package. Assess every structured alert below, recheck that each is still open, and reuse matching prior PR work only after inspecting its current diff and checks.",
        first.map_or_else(
            || {
                serde_json::json!({
                    "package": identity.package,
                    "manifest_path": identity.manifest_path,
                })
            },
            |alert| serde_json::json!(alert.details),
        ),
        Some(id.to_string()),
        template.created_at.as_deref().unwrap_or(""),
    );
    let (prefix, _) = task
        .description
        .split_once(SOURCE_MARKER)
        .expect("producer source");
    let mut source = source_json(&task.description).expect("producer source JSON");
    source["alerts"] = serde_json::json!(alerts);
    if let Some(complete) = source_json(&template.description)
        .and_then(|source| source.get("snapshot_complete").cloned())
    {
        source["snapshot_complete"] = complete;
    }
    task.description = format!("{prefix}{SOURCE_MARKER}{source}");
    // Candidate links and identity warnings are context, never source-comparison evidence.
    if let Some(suffix) = source_suffix(&template.description) {
        task.description.push_str(suffix);
    }
    task
}

fn canonical_source(task: &TeamTaskDto) -> Option<RemediationIdentity> {
    let (work_id, identity) = stored_identity(&task.description)?;
    (work_id == task.id && identity.work_id() == task.id).then_some(identity)
}

// Both admission and the CAS merge see a complete, order-independent observation per tuple.
pub(super) fn aggregate_remediation_tasks(tasks: Vec<TeamTaskDto>) -> Vec<TeamTaskDto> {
    let mut grouped =
        BTreeMap::<String, (TeamTaskDto, Vec<AlertObservation>, BTreeSet<String>)>::new();
    let mut other = Vec::new();
    for task in tasks {
        if canonical_source(&task).is_some()
            && let Some(alerts) = observations(&task.description)
        {
            let suffix = source_suffix(&task.description).unwrap_or("").to_string();
            grouped
                .entry(task.id.clone())
                .and_modify(|(template, all, context)| {
                    all.extend(alerts.clone());
                    context.insert(suffix.clone());
                    if task.description < template.description {
                        *template = task.clone();
                    }
                })
                .or_insert((task, alerts, BTreeSet::from([suffix])));
        } else {
            other.push(task);
        }
    }
    other.extend(grouped.into_values().map(|(mut task, alerts, context)| {
        let suffix_len = source_suffix(&task.description).map_or(0, str::len);
        task.description
            .truncate(task.description.len() - suffix_len);
        for suffix in context {
            task.description.push_str(&suffix);
        }
        with_observations(&task, &canonical_observations(alerts), &task.id)
    }));
    other
}

// An absent tuple is a withdrawal only after a successful, untruncated open-alert scan.
pub(super) fn remediation_snapshot(
    mut tasks: Vec<TeamTaskDto>,
    existing: &BTreeMap<String, TeamTaskDto>,
    repo: &str,
    complete: bool,
    now: &str,
) -> Vec<TeamTaskDto> {
    if complete {
        let mut seen = tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<BTreeSet<_>>();
        for task in existing.values() {
            let Some((work_id, identity)) = stored_identity(&task.description) else {
                continue;
            };
            if work_id != task.id
                || !task.id.starts_with("dependency-remediation-")
                || !identity.repo.eq_ignore_ascii_case(repo)
                || !seen.insert(identity.work_id())
            {
                continue;
            }
            let mut withdrawn = with_observations(task, &[], &identity.work_id());
            withdrawn.created_at = Some(now.to_string());
            tasks.push(withdrawn);
        }
    }
    let mut tasks = aggregate_remediation_tasks(tasks);
    for task in &mut tasks {
        if canonical_source(task).is_some() {
            let mut source = source_json(&task.description).expect("validated source");
            source["snapshot_complete"] = serde_json::json!(complete);
            replace_source(task, source);
        }
    }
    tasks
}

pub(super) enum RemediationUpdate {
    NotRemediation,
    Unchanged,
    Write(Box<TeamTaskDto>),
}

// Descriptions authorized by an assignment and completed run/PR links are immutable.
// New source inputs live in ordinary dependent backlog rows, not in that assignment.
pub(super) fn plan_remediation_update(
    incoming: &mut TeamTaskDto,
    existing: &[TeamTaskDto],
) -> RemediationUpdate {
    let Some(identity) = canonical_source(incoming) else {
        return RemediationUpdate::NotRemediation;
    };
    let Some(mut alerts) = observations(&incoming.description) else {
        return RemediationUpdate::NotRemediation;
    };
    let (matching_id, _) = match_remediation_task(incoming, |id| {
        existing
            .iter()
            .find(|task| task.id == id)
            .map(|task| task.description.as_str())
    });
    let revision_prefix = format!("{}-r-", identity.work_id());
    let family = existing
        .iter()
        .filter(|task| {
            task.id == matching_id
                || (task.id.starts_with(&revision_prefix)
                    && stored_identity(&task.description)
                        .is_some_and(|(id, stored)| id == task.id && stored == identity))
        })
        .collect::<Vec<_>>();
    let editable = |task: &&TeamTaskDto| {
        task.status == "pending" && task.run.is_none() && task.assignment_nonce.is_none()
    };
    let pending = family.iter().copied().rfind(editable);
    if source_json(&incoming.description).and_then(|source| {
        source
            .get("snapshot_complete")
            .and_then(serde_json::Value::as_bool)
    }) == Some(false)
        && let Some(current) = pending
    {
        let observed = alerts
            .iter()
            .map(|alert| alert.alert_number)
            .collect::<BTreeSet<_>>();
        alerts.extend(
            observations(&current.description)
                .unwrap_or_default()
                .into_iter()
                .filter(|alert| !observed.contains(&alert.alert_number)),
        );
        alerts = canonical_observations(alerts);
    }
    let retained = family
        .iter()
        .copied()
        .filter(|task| !editable(task))
        .collect::<Vec<_>>();
    let covered = retained
        .iter()
        .filter(|task| {
            stored_identity(&task.description)
                .is_some_and(|(id, stored)| id == task.id && stored == identity)
        })
        .flat_map(|task| observations(&task.description).unwrap_or_default())
        .map(|alert| alert.evidence_key())
        .collect::<BTreeSet<_>>();
    let novel = alerts
        .into_iter()
        .filter(|alert| !covered.contains(&alert.evidence_key()))
        .collect::<Vec<_>>();

    if let Some(current) = pending {
        let prior = observations(&current.description).unwrap_or_default();
        if prior
            .iter()
            .map(AlertObservation::evidence_key)
            .collect::<Vec<_>>()
            == novel
                .iter()
                .map(AlertObservation::evidence_key)
                .collect::<Vec<_>>()
        {
            return RemediationUpdate::Unchanged;
        }
        let mut refreshed = with_observations(incoming, &novel, &current.id);
        if let Some((_, history)) = current
            .description
            .split_once("\n\nChanged source follow-up.")
        {
            refreshed
                .description
                .push_str("\n\nChanged source follow-up.");
            refreshed.description.push_str(history);
        }
        let mut updated = current.clone();
        updated.title = refreshed.title;
        updated.description = refreshed.description;
        updated.review_required = true;
        if novel.is_empty() {
            updated.title = format!(
                "[Dependabot source withdrawn] {}: {}",
                identity.repo, identity.package
            );
            updated.status = "done".into();
            updated.done_at = incoming.created_at.clone();
            updated.description.push_str(
                "\n\nSource-only retirement: no new open findings remain in this observation. This is not evidence of a remediation, delivered PR, or successful run.",
            );
        }
        return RemediationUpdate::Write(Box::new(updated));
    }
    if novel.is_empty() {
        return RemediationUpdate::Unchanged;
    }
    let mut id = if family.is_empty() {
        matching_id
    } else {
        let evidence = novel
            .iter()
            .map(AlertObservation::evidence_key)
            .collect::<Vec<_>>();
        let digest = sha256(
            serde_json::json!([identity.work_id(), evidence])
                .to_string()
                .as_bytes(),
        );
        format!("{revision_prefix}{}", hex::encode(&digest[..6]))
    };
    // Never replace retained rows, including source-only retirements or malformed history.
    let mut collision = 0_u64;
    while existing.iter().any(|task| task.id == id) {
        collision += 1;
        let digest = sha256(serde_json::json!([id, collision]).to_string().as_bytes());
        id = format!("{revision_prefix}{}", hex::encode(&digest[..6]));
    }
    let mut followup = with_observations(incoming, &novel, &id);
    followup.depends_on = retained
        .iter()
        .filter(|task| task.status != "done")
        .map(|task| task.id.clone())
        .collect();
    if !family.is_empty() {
        followup.description.push_str(
            "\n\nChanged source follow-up. Prior assignments and completed PR evidence remain in these backlog tasks; inspect their retained runs before making changes:",
        );
        for task in &family {
            followup.description.push_str(&format!(
                "\n- {} (run: {})",
                task.id,
                task.run.as_deref().unwrap_or("none")
            ));
        }
    }
    RemediationUpdate::Write(Box::new(followup))
}

pub(super) fn description_matches_remediation(
    description: &str,
    repo: &str,
    manifest_path: Option<&str>,
    package: &str,
) -> bool {
    stored_identity(description).is_some_and(|(_, identity)| {
        identity == RemediationIdentity::new(repo, manifest_path, package)
    })
}

pub(super) fn note_candidate_pulls(task: &mut TeamTaskDto, pulls: &[&GithubPull]) {
    if pulls.is_empty() {
        return;
    }
    task.description.push_str(
        "\n\nOpen PR candidates mention this package or advisory; their titles are not \
         evidence of coverage. Before creating duplicate work, compare their actual diff \
         with this exact case-sensitive manifest and validate fixes and checks at the \
         current head SHA. Reuse or revise a genuinely matching PR rather than opening \
         another. Do not treat this remediation as delivered merely because a PR exists.",
    );
    for pull in pulls {
        task.description
            .push_str(&format!("\n#{} {}", pull.number, pull.html_url));
    }
}

// Resolve only incoming v2 work. Existing rows and their run/receipt links remain untouched.
pub(super) fn match_remediation_task<'a>(
    task: &mut TeamTaskDto,
    description_for_id: impl Fn(&str) -> Option<&'a str>,
) -> (String, Option<String>) {
    if !task.id.starts_with("dependency-remediation-v2-") {
        return (task.id.clone(), None);
    }
    let Some((source_work_id, identity)) = stored_identity(&task.description) else {
        return (task.id.clone(), None);
    };
    if source_work_id != task.id || identity.work_id() != task.id {
        return (task.id.clone(), None);
    }
    let legacy_id = identity.legacy_work_id();
    let Some(description) = description_for_id(&legacy_id) else {
        return (task.id.clone(), None);
    };
    match stored_identity(description) {
        Some((stored_work_id, stored)) if stored_work_id == legacy_id => {
            if stored == identity && description_for_id(&task.id).is_none() {
                return (legacy_id, None);
            }
            (task.id.clone(), None)
        }
        _ => {
            let warning = format!(
                "Remediation identity ambiguity: legacy task {legacy_id} lacks complete, attributable original repository/package/manifest metadata. Its history is preserved, but it is not evidence of delivery for {}. Track this versioned work separately; inspect the legacy run and receipts before making changes.",
                task.id
            );
            if !task.description.contains(&warning) {
                task.description.push_str("\n\n");
                task.description.push_str(&warning);
            }
            (task.id.clone(), Some(warning))
        }
    }
}

#[cfg(test)]
#[path = "remediation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "remediation_renewal_tests.rs"]
mod renewal_tests;
