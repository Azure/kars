// kars Bridge BFF — workspace-level, AGENT-AGNOSTIC communication channels.
//
// The user asked to move channel wiring (Telegram / Slack / Discord / WhatsApp)
// under the Connections tab, alongside GitHub, and out of the per-team envelope.
// A channel configured here applies to the whole WORKSPACE: the controller
// propagates the `kars-workspace-channels` secret into EVERY run sandbox —
// mission or team — so any agent can report over it, regardless of harness. A
// standing team may still layer its own channel secret on top.
//
// SECURITY: identical to the team-channel API — the token is written straight
// into a K8s Secret and NEVER logged or returned. GET only reveals which
// channels are enabled, never the token.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::Deserialize;

use crate::auth::Principal;

use crate::error::{AppError, AppResult};
use crate::routes::teams::{ChannelsDto, channel_env_keys, channels_from_keys};
use crate::state::AppState;

fn teams_runtime_names() -> (String, String, String, String) {
    (
        std::env::var("BRIDGE_INSTALL_NAMESPACE").unwrap_or_else(|_| "kars-system".into()),
        std::env::var("BRIDGE_TEAMS_SECRET_NAME").unwrap_or_else(|_| "kars-bridge-teams".into()),
        std::env::var("BRIDGE_TEAMS_GATEWAY_DEPLOYMENT")
            .unwrap_or_else(|_| "kars-bridge-teams-gateway".into()),
        std::env::var("BRIDGE_BFF_DEPLOYMENT").unwrap_or_else(|_| "kars-bridge-bff".into()),
    )
}

fn require_cluster(state: &AppState) -> AppResult<&crate::kars::cluster::Cluster> {
    state.cluster().ok_or(AppError::ClusterUnavailable)
}

#[derive(Debug, Deserialize)]
pub struct SetChannelRequest {
    /// Channel id: telegram | slack | discord | whatsapp | teams.
    pub channel: String,
    /// The channel's bot token / OAuth token. For whatsapp send "true".
    /// Not required when `channel == "teams"` (uses structured fields instead).
    #[serde(default)]
    pub token: String,
    /// Telegram only: comma-separated allowed numeric user IDs.
    #[serde(default)]
    pub allow_from: Option<String>,
    /// Teams only: structured credentials (separate fields, no composite strings).
    #[serde(default)]
    pub teams: Option<TeamsChannelCredentials>,
}

/// Teams channel structured credential fields — write-only; never returned.
#[derive(Debug, Deserialize)]
pub struct TeamsChannelCredentials {
    pub client_id: String,
    pub tenant_id: String,
    pub client_secret: String,
    /// JSON identity map: [{"entra_subject":"<oid>","bridge_subject":"<oidc-sub>","roles":["operator"],"name":"Alice"}]
    pub entra_role_map: String,
}

/// `GET /api/namespaces/:ns/channels` — which workspace channels are enabled.
pub async fn get_channels(
    State(state): State<AppState>,
    Path(ns): Path<String>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let keys = cluster
        .workspace_channel_keys(&ns)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&keys);
    Ok(Json(ChannelsDto {
        enabled,
        statuses: Vec::new(),
    }))
}

/// `POST /api/namespaces/:ns/channels` — enable/update a workspace channel. The
/// token is written into the workspace channel Secret and never echoed back.
/// Teams channel credentials require operator or admin role.
pub async fn set_channel(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path(ns): Path<String>,
    Json(b): Json<SetChannelRequest>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let keys = channel_env_keys(b.channel.as_str());
    if keys.is_empty() {
        return Err(AppError::BadRequest(format!(
            "unknown channel '{}': use telegram|slack|discord|whatsapp|teams",
            b.channel
        )));
    }

    // The identity map can bind Entra identities to immutable Bridge owner
    // subjects. Only an admin may create or replace that trust mapping.
    if b.channel == "teams" && !principal.roles.iter().any(|r| r == "admin") {
        return Err(AppError::Forbidden(
            "admin role required to configure Teams identity mapping".into(),
        ));
    }

    let mut data = std::collections::BTreeMap::new();

    if b.channel == "teams" {
        let teams = b.teams.ok_or_else(|| {
            AppError::BadRequest(
                "teams channel requires structured 'teams' credentials object".into(),
            )
        })?;
        if teams.client_id.trim().is_empty()
            || teams.tenant_id.trim().is_empty()
            || teams.client_secret.trim().is_empty()
            || teams.entra_role_map.trim().is_empty()
        {
            return Err(AppError::BadRequest(
                "all Teams credential fields (client_id, tenant_id, client_secret, entra_role_map) are required".into(),
            ));
        }
        // Server-side validation of the role map: reject operator/admin grants by
        // non-admin principals, and reject unknown role values entirely.
        const VALID_ROLES: &[&str] = &["user", "operator", "admin", "auditor"];
        let role_map: serde_json::Value = serde_json::from_str(teams.entra_role_map.trim())
            .map_err(|_| AppError::BadRequest("entra_role_map must be valid JSON".into()))?;
        let entries = role_map
            .as_array()
            .ok_or_else(|| AppError::BadRequest("entra_role_map must be a JSON array".into()))?;
        if entries.is_empty() {
            return Err(AppError::BadRequest(
                "entra_role_map must not be empty".into(),
            ));
        }
        for entry in entries {
            for field in ["entra_subject", "bridge_subject", "name"] {
                if entry
                    .get(field)
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .is_none_or(str::is_empty)
                {
                    return Err(AppError::BadRequest(format!(
                        "each role map entry must have a non-empty '{field}'"
                    )));
                }
            }
            let roles = entry
                .get("roles")
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    AppError::BadRequest("each role map entry must have a 'roles' array".into())
                })?;
            for role in roles {
                let role_str = role
                    .as_str()
                    .ok_or_else(|| AppError::BadRequest("role values must be strings".into()))?;
                if !VALID_ROLES.contains(&role_str) {
                    return Err(AppError::BadRequest(format!(
                        "invalid role '{role_str}': allowed values are user, operator, admin, auditor"
                    )));
                }
            }
        }
        // Teams credentials go in a DEDICATED Secret (kars-bridge-teams), NOT in
        // kars-workspace-channels which is propagated to sandbox pods.
        let mut teams_data = std::collections::BTreeMap::new();
        teams_data.insert("client-id".to_string(), teams.client_id.trim().to_string());
        teams_data.insert("tenant-id".to_string(), teams.tenant_id.trim().to_string());
        teams_data.insert(
            "client-secret".to_string(),
            teams.client_secret.trim().to_string(),
        );
        teams_data.insert(
            "entra-role-map".to_string(),
            teams.entra_role_map.trim().to_string(),
        );
        // Also store a cryptographically random BFF internal secret for gateway↔BFF auth
        let internal_secret = if let Some(existing) = state.teams_internal_secret() {
            existing.to_string()
        } else {
            use std::io::Read;
            let mut buf = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut buf))
                .map_err(|e| AppError::Internal(anyhow::anyhow!("CSPRNG failed: {e}")))?;
            hex::encode(buf)
        };
        teams_data.insert("bff-internal-secret".to_string(), internal_secret);
        let (namespace, secret, gateway, bff) = teams_runtime_names();
        cluster
            .write_dedicated_teams_secret(&namespace, &secret, teams_data)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
        cluster
            .reconcile_teams_deployments(&namespace, &gateway, &bff, true)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    } else {
        if b.token.trim().is_empty() {
            return Err(AppError::BadRequest("token is required".into()));
        }
        data.insert(keys[0].to_string(), b.token.trim().to_string());
        if b.channel == "telegram"
            && let Some(allow) = b
                .allow_from
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            data.insert("TELEGRAM_ALLOW_FROM".to_string(), allow.to_string());
        }
    }

    if b.channel != "teams" {
        cluster
            .merge_workspace_channel(&ns, data)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    }
    let after = cluster
        .workspace_channel_keys(&ns)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&after);
    Ok(Json(ChannelsDto {
        enabled,
        statuses: Vec::new(),
    }))
}

/// `DELETE /api/namespaces/:ns/channels/:channel` — disable a workspace channel
/// (removes its env keys; deletes the Secret when the last channel is removed).
/// For Teams: also deletes the dedicated kars-bridge-teams Secret to revoke all
/// credentials, and annotates the gateway Deployment to trigger a rollout (so the
/// gateway picks up the missing secret and stops processing).
pub async fn delete_channel(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, channel)): Path<(String, String)>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let keys = channel_env_keys(channel.as_str());
    if keys.is_empty() {
        return Err(AppError::BadRequest(format!(
            "unknown channel '{channel}': use telegram|slack|discord|whatsapp|teams"
        )));
    }
    // Teams disconnect requires operator/admin
    if channel == "teams"
        && !principal
            .roles
            .iter()
            .any(|r| r == "operator" || r == "admin")
    {
        return Err(AppError::Forbidden(
            "operator or admin role required to disconnect Teams integration".into(),
        ));
    }
    // For Teams: delete the dedicated secret and trigger gateway rollout
    if channel == "teams" {
        let (namespace, secret, gateway, bff) = teams_runtime_names();
        cluster
            .disable_dedicated_teams_secret(&namespace, &secret)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
        cluster
            .reconcile_teams_deployments(&namespace, &gateway, &bff, false)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    }
    let owned: Vec<String> = keys.iter().map(|s| s.to_string()).collect();
    if channel != "teams" {
        cluster
            .remove_workspace_channel_keys(&ns, &owned)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
    }
    let after = cluster
        .workspace_channel_keys(&ns)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&after);
    Ok(Json(ChannelsDto {
        enabled,
        statuses: Vec::new(),
    }))
}
