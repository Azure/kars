// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::{Extension, Path, State};
use serde::{Deserialize, Serialize};

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::kars::team::KarsTeam;
use crate::routes::tasks::require_cluster;

use super::require_owned_team;

// ─── Communication channels (part of a team's envelope) ──────────────────────
// A standing team can report to its operator over Telegram / Slack / Discord /
// WhatsApp. Tokens live ONLY in the K8s Secret `kars-team-channel-<team>`,
// propagated by the controller into each ephemeral run sandbox. SECURITY: the
// API is write-only for tokens — GET never returns a token, only which channels
// are enabled.

/// Map a channel id → the env keys the sandbox entrypoint reads for it.
pub(crate) fn channel_env_keys(channel: &str) -> &'static [&'static str] {
    match channel {
        "telegram" => &["TELEGRAM_BOT_TOKEN", "TELEGRAM_ALLOW_FROM"],
        "slack" => &["SLACK_BOT_TOKEN"],
        "discord" => &["DISCORD_BOT_TOKEN"],
        "whatsapp" => &["WHATSAPP_ENABLED"],
        // Teams uses a dedicated Secret (kars-bridge-teams), not workspace channels.
        // Only a non-secret marker key goes in workspace-channels for enabled detection.
        "teams" => &["TEAMS_ENABLED"],
        _ => &[],
    }
}

/// Derive which channels are enabled from the present secret keys (no values).
pub(crate) const SUPPORTED_CHANNELS: &[&str] =
    &["telegram", "slack", "discord", "whatsapp", "teams"];

pub(crate) fn channels_from_keys(keys: &[String]) -> Vec<String> {
    SUPPORTED_CHANNELS
        .iter()
        .copied()
        .filter(|ch| {
            // A channel is "enabled" if its primary token/flag key is present.
            let primary = channel_env_keys(ch).first().copied().unwrap_or("");
            keys.iter().any(|k| k == primary)
        })
        .map(String::from)
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelQualificationDto {
    pub channel: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChannelsDto {
    /// Channel ids currently enabled (e.g. ["telegram","slack"]).
    pub enabled: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<ChannelQualificationDto>,
}

async fn effective_team_route(
    cluster: &crate::kars::cluster::Cluster,
    team: &KarsTeam,
) -> Option<(String, String, String)> {
    let runtime = team
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.runtime.clone())
        .filter(|runtime| !runtime.is_empty())
        .unwrap_or_else(|| "OpenClaw".to_string());
    if let Some(model) = team
        .spec
        .blueprint
        .as_ref()
        .and_then(|blueprint| blueprint.model.as_ref())
    {
        return Some((runtime, model.provider.clone(), model.deployment.clone()));
    }
    let deployment = cluster.controller_default_model().await?;
    let provider = cluster.controller_provider().await.map(|(id, _, _)| id);
    Some((
        runtime,
        crate::routes::options::provider_for(&deployment, None, provider.as_deref()),
        deployment,
    ))
}

async fn channel_statuses_for_team(
    cluster: &crate::kars::cluster::Cluster,
    team: &KarsTeam,
    enabled: &[String],
) -> Vec<ChannelQualificationDto> {
    let route = effective_team_route(cluster, team).await;
    SUPPORTED_CHANNELS
        .iter()
        .copied()
        .map(|channel| {
            let enabled = enabled.iter().any(|configured| configured == channel);
            match route.as_ref() {
                Some((runtime, provider, deployment)) => {
                    let qualification = crate::routes::options::channel_adapter_qualified_for_route(
                        runtime,
                        provider,
                        deployment,
                        channel,
                    );
                    match qualification {
                        Ok(qualified) => ChannelQualificationDto {
                            channel: channel.to_string(),
                            enabled,
                            qualified: Some(qualified),
                            detail: Some(if qualified {
                                format!(
                                    "Retained channel-adapter evidence exists for {}.",
                                    crate::routes::options::route_label(
                                        runtime, provider, deployment
                                    )
                                )
                            } else {
                                format!(
                                    "No retained channel-adapter qualification matches {}. Credentials can be configured later, but generic route records do not prove this channel adapter.",
                                    crate::routes::options::route_label(
                                        runtime, provider, deployment
                                    )
                                )
                            }),
                        },
                        Err(error) => ChannelQualificationDto {
                            channel: channel.to_string(),
                            enabled,
                            qualified: None,
                            detail: Some(format!(
                                "Channel qualification could not be evaluated: {error}"
                            )),
                        },
                    }
                }
                None => ChannelQualificationDto {
                    channel: channel.to_string(),
                    enabled,
                    qualified: None,
                    detail: Some(
                        "The team has no effective runtime/model route yet, so channel qualification cannot be evaluated."
                            .into(),
                    ),
                },
            }
        })
        .collect()
}

#[derive(Debug, Deserialize)]
pub struct SetChannelRequest {
    /// Channel id: telegram | slack | discord | whatsapp.
    pub channel: String,
    /// The channel's bot token / OAuth token. For whatsapp send "true".
    pub token: String,
    /// Telegram only: comma-separated allowed numeric user IDs.
    #[serde(default)]
    pub allow_from: Option<String>,
}

/// `GET /api/namespaces/:ns/teams/:name/channels` — which channels are enabled.
pub async fn get_team_channels(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let keys = cluster
        .team_channel_keys(&ns, &name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&keys);
    Ok(Json(ChannelsDto {
        statuses: channel_statuses_for_team(cluster, &team, &enabled).await,
        enabled,
    }))
}

/// `POST /api/namespaces/:ns/teams/:name/channels` — enable/update a channel.
/// The token is written straight into the team's channel Secret and never
/// echoed back.
pub async fn set_team_channel(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
    Json(b): Json<SetChannelRequest>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let keys = channel_env_keys(b.channel.as_str());
    if keys.is_empty() {
        return Err(AppError::BadRequest(format!(
            "unknown channel '{}': use telegram|slack|discord|whatsapp",
            b.channel
        )));
    }
    if b.token.trim().is_empty() {
        return Err(AppError::BadRequest("token is required".into()));
    }
    let mut data = std::collections::BTreeMap::new();
    // whatsapp uses a presence flag, not a token.
    let primary = keys[0];
    data.insert(primary.to_string(), b.token.trim().to_string());
    if b.channel == "telegram"
        && let Some(allow) = b
            .allow_from
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        data.insert("TELEGRAM_ALLOW_FROM".to_string(), allow.to_string());
    }
    cluster
        .merge_team_channel(&ns, &name, data)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let after = cluster
        .team_channel_keys(&ns, &name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&after);
    Ok(Json(ChannelsDto {
        statuses: channel_statuses_for_team(cluster, &team, &enabled).await,
        enabled,
    }))
}

/// `DELETE /api/namespaces/:ns/teams/:name/channels/:channel` — disable a channel.
pub async fn delete_team_channel(
    State(state): State<crate::state::AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name, channel)): Path<(String, String, String)>,
) -> AppResult<Json<ChannelsDto>> {
    let cluster = require_cluster(&state)?;
    let team = require_owned_team(cluster, &ns, &name, &principal).await?;
    let keys: Vec<String> = channel_env_keys(channel.as_str())
        .iter()
        .map(|s| s.to_string())
        .collect();
    if keys.is_empty() {
        return Err(AppError::BadRequest(format!("unknown channel '{channel}'")));
    }
    cluster
        .remove_team_channel_keys(&ns, &name, &keys)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let after = cluster
        .team_channel_keys(&ns, &name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let enabled = channels_from_keys(&after);
    Ok(Json(ChannelsDto {
        statuses: channel_statuses_for_team(cluster, &team, &enabled).await,
        enabled,
    }))
}
