// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, State};
use kube::core::DynamicObject;
use serde::Serialize;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::policies::{ApplyCrdRequest, apply_err, apply_governance, delete_governance};
use super::{
    annotation, is_dns1123_label, name_of, ns_of, require_cluster, s, spec, status, upstream,
};

// Skill-admission annotation keys — the operator trust gate (§ skills workflow):
// a user-uploaded skill is only usable once an operator has scanned + approved
// it, which LOCKS the approval to the exact version digest at approval time.
// Any later change to the skill breaks the lock and returns it to review.
const ANN_REVIEW: &str = "kars.azure.com/skill-review";
const ANN_LOCKED_DIGEST: &str = "kars.azure.com/skill-locked-digest";
const ANN_APPROVED_BY: &str = "kars.azure.com/skill-approved-by";
const ANN_APPROVED_AT: &str = "kars.azure.com/skill-approved-at";

// ─── Skills & Profiles (predefined building blocks for customers) ────────────

#[derive(Debug, Serialize)]
pub struct SkillDto {
    pub name: String,
    pub namespace: String,
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
    // Usable only when explicitly approved and the lock still matches the live
    // digest. When the skill has no digest yet (not scanned), it can't be usable.
    let usable = review == "approved" && locked_digest.is_some() && locked_digest == version_digest;
    SkillDto {
        name: name_of(o),
        namespace: ns_of(o),
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
    let items = cluster.list_kind_all("KarsSkill").await.map_err(upstream)?;
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
    /// The skill PACKAGE files — flat filenames (SKILL.md + scripts). Stored as
    /// the `karsskill-<name>` ConfigMap and mounted into a granting sandbox.
    #[serde(default)]
    pub files: Vec<SkillFile>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SkillFile {
    /// Flat filename (no path separators) — e.g. `SKILL.md`, `triage.sh`.
    pub name: String,
    pub content: String,
}

/// `POST /api/skills` — USER skill submission. A team member uploads a skill
/// package; it lands as a `KarsSkill` that starts life PENDING REVIEW (never
/// usable until an operator scans + approves it). This is the user side of the
/// trust gate: users propose capability, operators vet + sign, then it's
/// grantable. The BFF never marks a user-submitted skill approved.
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
    // Validate + collect the package files. Standard Agent Skills use
    // subdirectories (scripts/, references/, assets/) referenced relatively from
    // SKILL.md. ConfigMap keys can't contain '/', so we accept relative paths
    // here and path-encode '/'→'__' only when writing the ConfigMap; the sandbox
    // entrypoint decodes them back on mount so the on-disk tree matches exactly.
    // A real skill package is at least a SKILL.md at the root.
    let mut files: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for f in &req.files {
        let fname = f.name.trim();
        let bad_segments = fname.split('/').any(|seg| {
            seg.is_empty()
                || !seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
        if fname.is_empty()
            || fname.starts_with('/')
            || fname.ends_with('/')
            || fname.contains("..")
            || fname.contains("__") // reserved as the CM path separator
            || fname.len() > 253
            || bad_segments
        {
            return Err(AppError::BadRequest(format!(
                "invalid skill file path '{fname}': use relative paths (letters, digits, . _ - and / for subdirs); no '..', no '__', no leading/trailing '/'"
            )));
        }
        files.insert(fname.to_string(), f.content.clone());
    }
    if !files.is_empty() {
        // A package the agent can actually USE must carry a SKILL.md — OpenClaw
        // auto-discovers `<name>/SKILL.md` and reads its frontmatter `description`
        // to know when to invoke the skill. Without it the files are dead weight.
        let skill_md = files.get("SKILL.md");
        match skill_md {
            None => {
                return Err(AppError::BadRequest(
                    "a skill package must include a SKILL.md — the agent discovers the skill from it".into(),
                ));
            }
            Some(md) if !md.contains("description:") => {
                return Err(AppError::BadRequest(
                    "SKILL.md must have YAML frontmatter with a `description:` — that's how the agent knows when to use the skill".into(),
                ));
            }
            _ => {}
        }
        spec["package"] = serde_json::json!(true);
        spec["files"] = serde_json::json!(files.keys().cloned().collect::<Vec<_>>());
        let configmap_data: std::collections::BTreeMap<String, String> = files
            .iter()
            .map(|(path, content)| (path.replace('/', "__"), content.clone()))
            .collect();
        let canonical = serde_json::to_vec(&configmap_data)
            .map_err(|e| AppError::Internal(anyhow::Error::new(e)))?;
        spec["packageDigest"] = serde_json::json!(format!(
            "sha256:{}",
            crate::providers::signing::sha256_hex(canonical)
        ));
    }
    let uploader = principal.name;
    let uploader_sub = principal.sub;
    let body = serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSkill",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { "app.kubernetes.io/managed-by": "kars-bridge" },
            // Explicitly PENDING — the operator trust gate must approve it before
            // it is usable. Never set review=approved on the user path.
            "annotations": {
                ANN_REVIEW: "pending",
                ANN_UPLOADED_BY: uploader,
                ANN_UPLOADED_BY_SUB: uploader_sub,
            },
        },
        "spec": spec,
    });
    let applied = cluster
        .apply_kind(&ns, "KarsSkill", body, false)
        .await
        .map_err(apply_err)?;
    // Persist the package files as the karsskill-<name> ConfigMap so the
    // controller can mount them into a granting sandbox. ConfigMap keys can't
    // contain '/', so subdirectory paths are encoded '/'→'__'; the sandbox
    // entrypoint decodes them back to the real tree on mount.
    if !files.is_empty() {
        let cm_files: std::collections::BTreeMap<String, String> = files
            .iter()
            .map(|(path, content)| (path.replace('/', "__"), content.clone()))
            .collect();
        let package_digest = spec
            .get("packageDigest")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("package digest missing")))?;
        cluster
            .write_skill_package(name, &cm_files, package_digest)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    }
    Ok(Json(to_skill(&applied)))
}

/// Locate a skill by name across namespaces, returning `(namespace, object)`.
async fn find_skill(
    cluster: &crate::kars::cluster::Cluster,
    name: &str,
) -> AppResult<(String, DynamicObject)> {
    let items = cluster.list_kind_all("KarsSkill").await.map_err(upstream)?;
    items
        .into_iter()
        .find(|o| name_of(o) == name)
        .map(|o| (ns_of(&o), o))
        .ok_or(AppError::NotFound)
}

/// `POST /api/operator/skills/:name/approve` — the operator admission gate.
/// Records the operator's approval and LOCKS it to the skill's current version
/// digest, after which users can assign the skill. Requires the skill to have
/// been scanned (a version digest present) and its attestation to have verified
/// — an operator can't approve a skill the controller hasn't validated. Any
/// later change to the skill breaks the lock and returns it to review.
pub async fn approve_skill(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(_req): Json<ApproveSkillRequest>,
) -> AppResult<Json<SkillDto>> {
    let cluster = require_cluster(&state)?;
    let (ns, obj) = find_skill(cluster, &name).await?;
    let uploader_subject = obj
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get(ANN_UPLOADED_BY_SUB))
        .cloned();
    if uploader_subject.as_deref() == Some(principal.sub.as_str()) {
        return Err(AppError::Forbidden(
            "skill submitter cannot approve their own package".into(),
        ));
    }
    let generation = obj.metadata.generation.unwrap_or_default();
    let observed_generation = status(&obj)
        .get("observedGeneration")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();
    if observed_generation != generation {
        return Err(AppError::Conflict(
            "skill changed after its last controller scan; wait for the current generation".into(),
        ));
    }
    let digest = s(status(&obj), "versionDigest").ok_or_else(|| {
        AppError::BadRequest(
            "skill has not been scanned yet (no version digest) — the controller must validate it before approval".into(),
        )
    })?;
    // Honest gate: don't let an operator approve a skill whose attestation the
    // controller could not verify.
    if status(&obj)
        .get("attestationVerified")
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        return Err(AppError::BadRequest(
            "skill attestation did not verify — cannot approve until the scan passes".into(),
        ));
    }
    let by = principal.name;
    let now = chrono::Utc::now().to_rfc3339();
    let updated = cluster
        .annotate_kind(
            &ns,
            "KarsSkill",
            &name,
            &[
                (ANN_REVIEW, Some("approved".into())),
                (ANN_LOCKED_DIGEST, Some(digest)),
                (ANN_APPROVED_BY, Some(by)),
                (ANN_APPROVED_AT, Some(now)),
            ],
        )
        .await
        .map_err(upstream)?;
    Ok(Json(to_skill(&updated)))
}

/// `POST /api/operator/skills/:name/revoke` — withdraw approval, returning the
/// skill to review (users immediately stop seeing it).
pub async fn revoke_skill(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<SkillDto>> {
    let cluster = require_cluster(&state)?;
    let (ns, _) = find_skill(cluster, &name).await?;
    let updated = cluster
        .annotate_kind(
            &ns,
            "KarsSkill",
            &name,
            &[
                (ANN_REVIEW, Some("pending".into())),
                (ANN_LOCKED_DIGEST, None),
                (ANN_APPROVED_BY, None),
                (ANN_APPROVED_AT, None),
            ],
        )
        .await
        .map_err(upstream)?;
    Ok(Json(to_skill(&updated)))
}

#[derive(Debug, serde::Deserialize)]
pub struct ApproveSkillRequest {}

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
