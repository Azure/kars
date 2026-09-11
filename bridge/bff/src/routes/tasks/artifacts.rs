use axum::extract::{Extension, Path, State};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::require_owned_task_or_output;
use crate::state::AppState;

use super::evidence::{ARTIFACT_PREVIEW_TOTAL_BYTES, artifact_preview};
use super::{MissionArtifactDto, require_cluster};

/// Sanitize a filename to the ConfigMap key form the controller uses (alnum,
/// '-', '_', '.') so the manifest name can look up its stored content.
fn artifact_key(name: &str) -> String {
    let k: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if k.is_empty() { "artifact".into() } else { k }
}

/// Best-effort content type from a filename extension, so a downloaded artifact
/// opens sensibly in the browser instead of forcing a save dialog for text.
fn artifact_content_type(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("md" | "markdown" | "txt" | "log") => "text/markdown; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("yaml" | "yml") => "application/yaml; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// `GET /api/tasks/:ns/:name/artifact/:file` — Bridge-native artifact fetch.
/// Streams one artifact file's bytes (text from `data`, binary from
/// `binaryData`) so operators download deliverables in-product, never via
/// `kubectl`. Inline for previewable types; attachment otherwise.
pub async fn download_artifact(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, file)): Path<(String, String, String)>,
) -> AppResult<axum::response::Response> {
    use axum::http::header;
    let cluster = require_cluster(&state)?;
    require_owned_task_or_output(cluster, &ns, &name, &principal).await?;
    let key = artifact_key(&file);
    let (bytes, is_binary) = cluster
        .read_mission_artifact_bytes(&name, &key)
        .await
        .ok_or(AppError::NotFound)?;
    let ctype = artifact_content_type(&file);
    // Inline-render text/known media; force a download for opaque binaries.
    let disposition = if is_binary && ctype == "application/octet-stream" {
        format!("attachment; filename=\"{key}\"")
    } else {
        format!("inline; filename=\"{key}\"")
    };
    axum::response::Response::builder()
        .header(header::CONTENT_TYPE, ctype)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "private, max-age=60")
        .body(axum::body::Body::from(bytes))
        .map_err(|e| AppError::Upstream(e.to_string()))
}

/// Merge a mission's artifact manifest (names + sizes, from the output
/// ConfigMap) with the text contents stored in the companion artifacts
/// ConfigMap. Binary artifacts appear in the manifest but carry `content:
/// None`. Returns an empty set honestly when the mission produced no artifacts.
pub(super) async fn build_artifact_set(
    cluster: &crate::kars::cluster::Cluster,
    name: &str,
    output_data: Option<&std::collections::BTreeMap<String, String>>,
) -> Vec<MissionArtifactDto> {
    let manifest_json = output_data.and_then(|d| d.get("artifacts").cloned());
    let contents = cluster
        .read_mission_artifacts(name)
        .await
        .unwrap_or_default();

    // Prefer the manifest (authoritative order + sizes + binary entries); fall
    // back to whatever text artifacts are stored if no manifest is present.
    if let Some(mj) = manifest_json
        && let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&mj)
    {
        let mut seen = std::collections::HashSet::new();
        let mut preview_budget = ARTIFACT_PREVIEW_TOTAL_BYTES;
        return entries
            .into_iter()
            .filter_map(|e| {
                let fname = e.get("name")?.as_str()?.to_string();
                // The manifest can list the same file twice (e.g. an artifact
                // recorded by both the run harness and the harvest step). Keep
                // the first — duplicates crash the UI's name-keyed lists.
                if !seen.insert(fname.clone()) {
                    return None;
                }
                let size_bytes = e.get("size_bytes").and_then(|v| v.as_i64());
                let (content, content_bytes, content_truncated, full_content) = artifact_preview(
                    contents.get(&artifact_key(&fname)).cloned(),
                    &mut preview_budget,
                );
                Some(MissionArtifactDto {
                    name: fname,
                    size_bytes,
                    content,
                    content_bytes,
                    content_truncated,
                    source_agent: e
                        .get("source_agent")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    source_path: e
                        .get("source_path")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    digest: e.get("digest").and_then(|v| v.as_str()).map(str::to_string),
                    full_content,
                })
            })
            .collect();
    }

    let mut preview_budget = ARTIFACT_PREVIEW_TOTAL_BYTES;
    contents
        .into_iter()
        .map(|(k, v)| {
            let size_bytes = v.len() as i64;
            let (content, content_bytes, content_truncated, full_content) =
                artifact_preview(Some(v), &mut preview_budget);
            MissionArtifactDto {
                size_bytes: Some(size_bytes),
                name: k,
                content,
                content_bytes,
                content_truncated,
                source_agent: None,
                source_path: None,
                digest: None,
                full_content,
            }
        })
        .collect()
}
