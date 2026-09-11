// kars Bridge BFF — remediation identity and compatibility with persisted intake.

use crate::providers::signing::sha256;

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

fn stored_identity(description: &str) -> Option<(String, RemediationIdentity)> {
    let (_, source) = description.split_once(SOURCE_MARKER)?;
    if source.contains(SOURCE_MARKER) {
        return None;
    }
    // Coverage notes may follow the original JSON. Read that object, not prose or substrings.
    let source = serde_json::Deserializer::from_str(source)
        .into_iter::<serde_json::Value>()
        .next()?
        .ok()?;
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
