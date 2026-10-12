// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, State};
use kube::core::DynamicObject;
use serde::Serialize;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::skill_package as package;
use crate::state::AppState;

use super::policies::{ApplyCrdRequest, apply_err, apply_governance, delete_governance};
use super::{
    annotation, is_dns1123_label, name_of, ns_of, require_cluster, s, spec, status, upstream,
};

const ANN_REVIEW: &str = package::REVIEW;
const ANN_LOCKED_DIGEST: &str = package::LOCKED_DIGEST;
const ANN_APPROVED_BY: &str = "kars.azure.com/skill-approved-by";
const ANN_APPROVED_AT: &str = "kars.azure.com/skill-approved-at";

#[cfg(test)]
#[path = "skills_profiles_tests.rs"]
mod tests;

// ─── Skills & Profiles (predefined building blocks for customers) ────────────

#[derive(Debug, Serialize)]
pub struct SkillDto {
    pub name: String,
    pub namespace: String,
    pub uid: Option<String>,
    pub generation: Option<i64>,
    pub validated: bool,
    pub validation_detail: Option<String>,
    pub version: Option<String>,
    pub summary: Option<String>,
    pub bounding_policy: Option<String>,
    pub phase: Option<String>,
    pub version_digest: Option<String>,
    pub attestation_verified: Option<bool>,
    // ── Operator trust gate ──────────────────────────────────────────────────
    /// Admission verdict: "approved" once an operator has signed off, else the
    /// skill is treated as pending review.
    pub review: String,
    /// The version digest the approval is locked to (from status at approval).
    pub locked_digest: Option<String>,
    pub approved_by: Option<String>,
    pub approved_at: Option<String>,
    /// True when approved AND the locked digest still matches the current
    /// version digest — i.e. usable by users. False if never approved or the
    /// skill changed since approval (lock broken → back to review).
    pub usable: bool,
    /// Raw `spec` for Edit-form prefill.
    pub spec: serde_json::Value,
}

fn to_skill(o: &DynamicObject) -> SkillDto {
    let sp = spec(o);
    let version_digest = s(status(o), "versionDigest");
    let review = annotation(o, ANN_REVIEW).unwrap_or_else(|| "pending".into());
    let locked_digest = annotation(o, ANN_LOCKED_DIGEST);
    let usable = package::approved_digest(&serde_json::json!(o)).is_ok();
    let validation = package::validated_digest(&serde_json::json!(o));
    SkillDto {
        name: name_of(o),
        namespace: ns_of(o),
        uid: o.metadata.uid.clone(),
        generation: o.metadata.generation,
        validated: validation.is_ok(),
        validation_detail: validation.err().or_else(|| s(status(o), "detail")),
        version: s(sp, "version"),
        summary: s(sp, "summary"),
        bounding_policy: s(sp, "boundingPolicy"),
        phase: s(status(o), "phase"),
        version_digest,
        attestation_verified: status(o)
            .get("attestationVerified")
            .and_then(|v| v.as_bool()),
        review,
        locked_digest,
        approved_by: annotation(o, ANN_APPROVED_BY),
        approved_at: annotation(o, ANN_APPROVED_AT),
        usable,
        spec: sp.clone(),
    }
}

#[derive(Debug, Serialize, serde::Deserialize, Clone)]
pub struct McpProfileDto {
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    /// Names of the operator-vetted McpServers this profile bundles.
    pub servers: Vec<String>,
}

/// `GET /api/operator/mcp-profiles` — the operator-curated MCP bundles users
/// can pick from (a named, vetted set of McpServers, so users compose from
/// approved groupings rather than assembling servers one by one).
pub async fn list_mcp_profiles(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<McpProfileDto>>> {
    let cluster = require_cluster(&state)?;
    let raw = cluster.read_mcp_profiles().await;
    let profiles: Vec<McpProfileDto> = serde_json::from_str(&raw).unwrap_or_default();
    Ok(Json(profiles))
}

/// `PUT /api/operator/mcp-profiles` — upsert a profile by name. Validates that
/// every referenced server is a real McpServer on the cluster, so a profile can
/// never bundle a non-existent (unvetted) server.
pub async fn put_mcp_profile(
    State(state): State<AppState>,
    Json(req): Json<McpProfileDto>,
) -> AppResult<Json<Vec<McpProfileDto>>> {
    let cluster = require_cluster(&state)?;
    if req.name.trim().is_empty() {
        return Err(AppError::BadRequest("profile name is required".into()));
    }
    // Real McpServers on the cluster — the vetted universe a profile may draw from.
    let known: std::collections::BTreeSet<String> = cluster
        .list_kind_all("McpServer")
        .await
        .map_err(upstream)?
        .iter()
        .map(name_of)
        .collect();
    for s in &req.servers {
        if !known.contains(s) {
            return Err(AppError::BadRequest(format!(
                "server '{s}' is not a registered McpServer — vet it first"
            )));
        }
    }
    let raw = cluster.read_mcp_profiles().await;
    let mut profiles: Vec<McpProfileDto> = serde_json::from_str(&raw).unwrap_or_default();
    profiles.retain(|p| p.name != req.name);
    profiles.push(req);
    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    let json = serde_json::to_string(&profiles).unwrap_or_else(|_| "[]".into());
    cluster
        .write_mcp_profiles(&json)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(profiles))
}

/// `DELETE /api/operator/mcp-profiles/:name` — remove a profile.
pub async fn delete_mcp_profile(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<Vec<McpProfileDto>>> {
    let cluster = require_cluster(&state)?;
    let raw = cluster.read_mcp_profiles().await;
    let mut profiles: Vec<McpProfileDto> = serde_json::from_str(&raw).unwrap_or_default();
    profiles.retain(|p| p.name != name);
    let json = serde_json::to_string(&profiles).unwrap_or_else(|_| "[]".into());
    cluster
        .write_mcp_profiles(&json)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(profiles))
}

pub async fn list_skills(State(state): State<AppState>) -> AppResult<Json<Vec<SkillDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind("kars-system", "KarsSkill")
        .await
        .map_err(upstream)?;
    let mut dtos: Vec<SkillDto> = items.iter().map(to_skill).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

/// Annotation recording who uploaded a user-submitted skill (provenance for the
/// operator reviewing it).
const ANN_UPLOADED_BY: &str = "kars.azure.com/skill-uploaded-by";
const ANN_UPLOADED_BY_SUB: &str = "kars.azure.com/skill-uploaded-by-sub";

#[derive(serde::Deserialize)]
pub struct SubmitSkillRequest {
    /// DNS-1123 object name (kebab-case).
    pub name: String,
    pub display_name: Option<String>,
    pub version: String,
    pub summary: String,
    /// The bounding tool policy — must be one the operator already vetted; it
    /// caps what the skill's recipe can do. Users pick from the approved set.
    pub bounding_policy: String,
    pub recipe: Option<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    /// UTF-8 package files, including relative subdirectories.
    #[serde(default)]
    pub files: Vec<SkillFile>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillFile {
    /// Relative path, such as `SKILL.md` or `references/checklist.md`.
    pub name: String,
    pub content: String,
}

/// `POST /api/skills` — USER skill submission. A team member uploads a skill
/// package; it lands as a `KarsSkill` that starts life PENDING REVIEW (never
/// usable until independent operator review). The BFF does not claim a malware
/// scan or cryptographic attestation verification.
pub async fn submit_skill(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(req): Json<SubmitSkillRequest>,
) -> AppResult<Json<SkillDto>> {
    let cluster = require_cluster(&state)?;
    let name = req.name.trim();
    if !is_dns1123_label(name) {
        return Err(AppError::BadRequest(
            "skill name must be a DNS-1123 label (lowercase letters, digits, hyphens)".into(),
        ));
    }
    if req.version.trim().is_empty() {
        return Err(AppError::BadRequest("version is required".into()));
    }
    if req.summary.trim().len() < 8 {
        return Err(AppError::BadRequest("a real summary is required".into()));
    }
    if req.bounding_policy.trim().is_empty() {
        return Err(AppError::BadRequest(
            "a bounding tool policy is required — it caps what the skill may do".into(),
        ));
    }
    let ns = "kars-system".to_string();
    let mut spec = serde_json::json!({
        "version": req.version.trim(),
        "summary": req.summary.trim(),
        "boundingPolicy": req.bounding_policy.trim(),
    });
    if let Some(dn) = req.display_name.as_ref().filter(|s| !s.trim().is_empty()) {
        spec["displayName"] = serde_json::json!(dn.trim());
    }
    if let Some(r) = req.recipe.as_ref().filter(|s| !s.trim().is_empty()) {
        spec["recipe"] = serde_json::json!(r.trim());
    }
    if !req.mcp_servers.is_empty() {
        spec["mcpServers"] = serde_json::json!(req.mcp_servers);
    }
    let mut files: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for f in &req.files {
        let fname = &f.name;
        if !package::valid_path(fname) || files.contains_key(fname) {
            return Err(AppError::BadRequest(format!(
                "invalid or duplicate skill path '{fname}': use a unique relative path; no '.', '..', '__', empty segments or whitespace"
            )));
        }
        files.insert(fname.to_string(), f.content.clone());
    }
    let cm_files: std::collections::BTreeMap<String, String> = files
        .iter()
        .map(|(path, content)| (path.replace('/', "__"), content.clone()))
        .collect();
    if !files.is_empty() {
        package::validate_manifest(&files.keys().cloned().collect::<Vec<_>>())
            .map_err(AppError::BadRequest)?;
        spec["package"] = serde_json::json!(true);
        spec["files"] = serde_json::json!(files.keys().cloned().collect::<Vec<_>>());
        spec["packageDigest"] =
            serde_json::json!(package::package_digest(&cm_files).map_err(AppError::BadRequest)?);
    }
    let applied = save_skill(cluster, name, &ns, spec, &principal, true).await?;
    cluster
        .write_skill_package(&applied, &cm_files)
        .await
        .map_err(|e| {
            AppError::Upstream(format!(
                "Skill saved pending review, but package persistence failed: {e}"
            ))
        })?;
    Ok(Json(to_skill(&applied)))
}

async fn save_skill(
    cluster: &crate::kars::cluster::Cluster,
    name: &str,
    namespace: &str,
    spec: serde_json::Value,
    principal: &Principal,
    submitter_only: bool,
) -> AppResult<DynamicObject> {
    if namespace != "kars-system" || !package::valid_name(name) {
        return Err(AppError::BadRequest(
            "Skills require a DNS label in kars-system".into(),
        ));
    }
    package::validate_spec(&spec).map_err(AppError::BadRequest)?;
    let existing = cluster
        .get_kind(namespace, "KarsSkill", name)
        .await
        .map_err(upstream)?;
    if let Some(object) = &existing {
        if object.metadata.deletion_timestamp.is_some() {
            return Err(AppError::Conflict("Skill is terminating".into()));
        }
        if submitter_only
            && annotation(object, ANN_UPLOADED_BY_SUB).as_deref() != Some(&principal.sub)
        {
            return Err(AppError::Forbidden(
                "Only the original submitter may replace this skill package".into(),
            ));
        }
    }
    let mut body = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSkill",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": { "app.kubernetes.io/managed-by": "kars-bridge" },
            "annotations": {
                ANN_REVIEW: "pending",
                ANN_UPLOADED_BY: principal.name,
                ANN_UPLOADED_BY_SUB: principal.sub,
            },
        },
        "spec": spec,
    });
    match existing {
        Some(object) => {
            body["metadata"]["uid"] = serde_json::json!(object.metadata.uid);
            body["metadata"]["resourceVersion"] =
                serde_json::json!(object.metadata.resource_version);
            crate::kars::execution_plans::identity_fence(&object.metadata)
                .map_err(AppError::Conflict)?;
            // Replace the submitted spec, including omitted optional fields,
            // without SSA ownership conflicts with prior creation or review.
            if let Some(previous) = object.data["spec"].as_object() {
                for key in previous.keys() {
                    if body["spec"].get(key).is_none() {
                        body["spec"][key] = serde_json::Value::Null;
                    }
                }
            }
            for key in [
                ANN_LOCKED_DIGEST,
                ANN_APPROVED_BY,
                ANN_APPROVED_AT,
                package::LOCKED_UID,
                package::LOCKED_GENERATION,
                package::APPROVED_BY_SUB,
            ] {
                body["metadata"]["annotations"][key] = serde_json::Value::Null;
            }
            cluster
                .merge_patch_kind(namespace, "KarsSkill", name, body)
                .await
                .map_err(review_error)
        }
        None => cluster
            .create_kind(namespace, "KarsSkill", body)
            .await
            .map_err(apply_err),
    }
}

pub(super) async fn edit_skill(
    cluster: &crate::kars::cluster::Cluster,
    principal: &Principal,
    req: ApplyCrdRequest,
) -> AppResult<Json<serde_json::Value>> {
    let applied = save_skill(
        cluster,
        req.name.trim(),
        req.namespace.as_deref().unwrap_or("kars-system"),
        req.spec,
        principal,
        false,
    )
    .await?;
    if spec(&applied)
        .get("package")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        cluster
            .write_skill_package(&applied, &Default::default())
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    }
    Ok(Json(
        serde_json::json!({"applied":true,"kind":"KarsSkill","name":name_of(&applied),
        "namespace":ns_of(&applied),"note":"Saved pending independent review of the current content."}),
    ))
}

async fn find_skill(
    cluster: &crate::kars::cluster::Cluster,
    name: &str,
) -> AppResult<(String, DynamicObject)> {
    cluster
        .get_kind("kars-system", "KarsSkill", name)
        .await
        .map_err(upstream)?
        .map(|o| ("kars-system".into(), o))
        .ok_or(AppError::NotFound)
}

pub async fn get_skill_package(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    let (_, object) = find_skill(cluster, &name).await?;
    let files = cluster
        .skill_package_files(&object)
        .await
        .map_err(|e| AppError::Conflict(e.to_string()))?;
    Ok(Json(
        serde_json::json!({"skill":to_skill(&object),"files":files}),
    ))
}

/// `POST /api/operator/skills/:name/approve` — the operator admission gate.
/// Records the operator's approval and LOCKS it to the skill's current version
/// digest, after which users can assign the skill. Requires the skill to have
/// current controller validation. This is manual review, not a malware scan or
/// a cryptographic attestation. Mutation is conditional on the inspected object.
pub async fn approve_skill(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(req): Json<ApproveSkillRequest>,
) -> AppResult<Json<SkillDto>> {
    let cluster = require_cluster(&state)?;
    let (ns, obj) = find_skill(cluster, &name).await?;
    let uploader_subject = obj
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANN_UPLOADED_BY_SUB))
        .cloned();
    if uploader_subject.as_deref().is_none_or(str::is_empty)
        || uploader_subject.as_deref() == Some(principal.sub.as_str())
    {
        return Err(AppError::Forbidden(
            "skill submitter cannot approve their own package".into(),
        ));
    }
    let digest = package::validated_digest(&serde_json::json!(obj)).map_err(AppError::Conflict)?;
    if obj.metadata.uid.as_deref() != Some(req.uid.as_str())
        || obj.metadata.generation != Some(req.generation)
        || digest != req.version_digest
    {
        return Err(AppError::Conflict(
            "Skill changed since the displayed review; inspect the current package".into(),
        ));
    }
    cluster
        .skill_package_files(&obj)
        .await
        .map_err(|e| AppError::Conflict(e.to_string()))?;
    let by = principal.name;
    let now = chrono::Utc::now().to_rfc3339();
    let updated = cluster
        .merge_patch_kind(
            &ns,
            "KarsSkill",
            &name,
            serde_json::json!({
                "metadata": {
                    "uid": obj.metadata.uid, "resourceVersion": obj.metadata.resource_version,
                    "annotations": {
                        ANN_REVIEW:"approved", ANN_LOCKED_DIGEST:digest,
                        (package::LOCKED_UID):obj.metadata.uid,
                        (package::LOCKED_GENERATION):obj.metadata.generation.map(|g| g.to_string()),
                        (package::APPROVED_BY_SUB):principal.sub,
                        ANN_APPROVED_BY:by, ANN_APPROVED_AT:now
                    }
                }
            }),
        )
        .await
        .map_err(review_error)?;
    Ok(Json(to_skill(&updated)))
}

/// `POST /api/operator/skills/:name/revoke` — withdraw approval, returning the
/// skill to review (users immediately stop seeing it).
pub async fn revoke_skill(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(req): Json<RevokeSkillRequest>,
) -> AppResult<Json<SkillDto>> {
    let cluster = require_cluster(&state)?;
    let (ns, obj) = find_skill(cluster, &name).await?;
    if obj.metadata.uid.as_deref() != Some(req.uid.as_str()) {
        return Err(AppError::Conflict(
            "Skill was replaced since it was displayed".into(),
        ));
    }
    let updated = cluster
        .merge_patch_kind(
            &ns,
            "KarsSkill",
            &name,
            serde_json::json!({
                "metadata": {
                    "uid":obj.metadata.uid, "resourceVersion":obj.metadata.resource_version,
                    "annotations":{
                        ANN_REVIEW:"pending", ANN_LOCKED_DIGEST:null,
                        (package::LOCKED_UID):null, (package::LOCKED_GENERATION):null,
                        (package::APPROVED_BY_SUB):null, ANN_APPROVED_BY:null, ANN_APPROVED_AT:null
                    }
                }
            }),
        )
        .await
        .map_err(review_error)?;
    Ok(Json(to_skill(&updated)))
}

fn review_error(error: kube::Error) -> AppError {
    if matches!(&error, kube::Error::Api(status) if status.code == 409) {
        AppError::Conflict("Skill changed during review; refresh and inspect it again".into())
    } else {
        upstream(error)
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ApproveSkillRequest {
    pub uid: String,
    pub generation: i64,
    pub version_digest: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct RevokeSkillRequest {
    pub uid: String,
}

#[derive(Debug, Serialize)]
pub struct ProfileRoleDto {
    pub name: String,
    pub system_prompt: Option<String>,
    pub skills: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileDto {
    pub name: String,
    pub namespace: String,
    pub domain: Option<String>,
    pub phase: Option<String>,
    pub template_digest: Option<String>,
    // Instantiation fields — so the team composer can prefill a whole team from
    // a profile (the profile is a vetted org template, not a dead-end record).
    pub display_name: Option<String>,
    pub charter_template: Option<String>,
    pub tier: Option<i32>,
    pub tool_policy: Option<String>,
    pub knowledge_commons: Option<String>,
    pub roles: Vec<ProfileRoleDto>,
    /// Raw spec for Edit-form prefill.
    pub spec: serde_json::Value,
}

fn to_profile(o: &DynamicObject) -> ProfileDto {
    let sp = spec(o);
    let roles = sp
        .get("roles")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let name = r.get("name")?.as_str()?.to_string();
                    Some(ProfileRoleDto {
                        name,
                        system_prompt: r
                            .get("systemPrompt")
                            .and_then(|s| s.as_str())
                            .map(String::from),
                        skills: r
                            .get("skills")
                            .and_then(|s| s.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    ProfileDto {
        name: name_of(o),
        namespace: ns_of(o),
        domain: s(sp, "domain"),
        phase: s(status(o), "phase"),
        template_digest: s(status(o), "templateDigest"),
        display_name: s(sp, "displayName"),
        charter_template: s(sp, "charterTemplate"),
        tier: sp
            .get("defaultEnvelope")
            .and_then(|e| e.get("tier"))
            .and_then(|t| t.as_i64())
            .map(|t| t as i32),
        tool_policy: s(sp, "toolPolicy"),
        knowledge_commons: s(sp, "knowledgeCommons"),
        roles,
        spec: sp.clone(),
    }
}

pub async fn list_profiles(State(state): State<AppState>) -> AppResult<Json<Vec<ProfileDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster
        .list_kind_all("KarsProfile")
        .await
        .map_err(upstream)?;
    let mut dtos: Vec<ProfileDto> = items.iter().map(to_profile).collect();
    dtos.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(dtos))
}

/// `PUT /api/operator/profiles` — author/edit a `KarsProfile` (SSA).
pub async fn put_profile(
    State(state): State<AppState>,
    Json(req): Json<ApplyCrdRequest>,
) -> AppResult<Json<serde_json::Value>> {
    apply_governance(require_cluster(&state)?, "KarsProfile", req).await
}

/// `DELETE /api/operator/profiles/:name` — remove a `KarsProfile`.
pub async fn delete_profile(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    delete_governance(require_cluster(&state)?, "KarsProfile", &name, None).await
}
