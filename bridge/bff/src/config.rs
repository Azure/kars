// Copyright (c) Pal Lakatos-Toth.
// kars Bridge BFF — runtime configuration loaded from the environment.
//
// The BFF is the only process that holds privileged cluster access and
// (later) the Entra confidential-client secret. Every value here is
// deployment configuration, never a hardcoded credential.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Fully-resolved BFF configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub bind_addr: SocketAddr,
    /// Allowed browser origin for the Next.js web app (CORS).
    pub web_origin: String,
    /// Log filter directive (`RUST_LOG`-style), e.g. `info,kars_bridge_bff=debug`.
    pub log_filter: String,
    /// Emit logs as JSON (production) vs. pretty (local dev).
    pub log_json: bool,
    /// Default namespace the BFF scopes readiness + listings to.
    pub default_namespace: String,
    /// Optional bearer token guarding MUTATING endpoints. When set, every state-
    /// changing request must present `Authorization: Bearer <token>`. When unset,
    /// mutations are allowed (dev) but a startup warning is logged.
    pub api_token: Option<String>,
    /// HS256 key shared only with the Bridge web server. When configured, every
    /// API request (except health/readiness) must carry the user's signed Bridge
    /// session in `X-Kars-Principal-Token`; route authorization is enforced from
    /// its immutable role claims.
    pub principal_secret: Option<String>,
    /// Shared secret for the Teams gateway internal decision endpoint.
    pub teams_internal_secret: Option<String>,
    /// Entra→Bridge role map JSON for server-side principal resolution.
    /// Parsed from BRIDGE_TEAMS_ENTRA_ROLE_MAP.
    /// Format: [{"entra_subject":"<oid>","bridge_subject":"<oidc-sub>","roles":["operator"],"name":"Alice"}]
    pub teams_entra_role_map: Vec<(String, String, Vec<String>, String)>,
    /// How often the background engineering-intake worker scans for due sources.
    /// Individual teams retain their own durable poll interval.
    pub engineering_poller_interval_seconds: u64,
}

impl Config {
    /// Load configuration from the environment, applying safe defaults.
    ///
    /// Returns an error only when a provided value is malformed — absence
    /// of an optional variable falls back to a documented default.
    pub fn from_env() -> anyhow::Result<Self> {
        let port: u16 = parse_env("BRIDGE_BFF_PORT", 8081)?;
        let host: IpAddr = match std::env::var("BRIDGE_BFF_HOST") {
            Ok(v) => v
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid BRIDGE_BFF_HOST `{v}`: {e}"))?,
            Err(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        };

        let web_origin = std::env::var("BRIDGE_WEB_ORIGIN")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());
        let log_filter =
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,kars_bridge_bff=debug".to_string());
        let log_json = parse_env("BRIDGE_LOG_JSON", false)?;
        let default_namespace =
            std::env::var("BRIDGE_DEFAULT_NAMESPACE").unwrap_or_else(|_| "kars-system".to_string());
        let api_token = std::env::var("BRIDGE_API_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());
        let principal_secret = std::env::var("BRIDGE_PRINCIPAL_SECRET")
            .ok()
            .filter(|t| !t.is_empty());
        let teams_internal_secret = std::env::var("BRIDGE_TEAMS_INTERNAL_SECRET")
            .ok()
            .filter(|t| !t.is_empty());
        let teams_entra_role_map = std::env::var("BRIDGE_TEAMS_ENTRA_ROLE_MAP")
            .ok()
            .filter(|t| !t.is_empty())
            .map(|raw| parse_entra_role_map(&raw))
            .unwrap_or_default();
        let engineering_poller_interval_seconds =
            parse_env("BRIDGE_ENGINEERING_POLLER_SECONDS", 60_u64)?;
        if engineering_poller_interval_seconds < 15 {
            anyhow::bail!("BRIDGE_ENGINEERING_POLLER_SECONDS must be at least 15");
        }

        Ok(Self {
            bind_addr: SocketAddr::new(host, port),
            web_origin,
            log_filter,
            log_json,
            default_namespace,
            api_token,
            principal_secret,
            teams_internal_secret,
            teams_entra_role_map,
            engineering_poller_interval_seconds,
        })
    }
}

/// Parse an environment variable into `T`, returning `default` when unset.
fn parse_env<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(v) => v
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid {key} `{v}`: {e}")),
        Err(_) => Ok(default),
    }
}

/// Parse the Entra→Bridge role map from JSON.
/// Returns (entra_subject, bridge_subject, bridge_roles, display_name) tuples.
fn parse_entra_role_map(raw: &str) -> Vec<(String, String, Vec<String>, String)> {
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("BRIDGE_TEAMS_ENTRA_ROLE_MAP is not valid JSON: {e}");
            return Vec::new();
        }
    };
    let Some(entries) = parsed.as_array() else {
        tracing::warn!("BRIDGE_TEAMS_ENTRA_ROLE_MAP is not a JSON array");
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let entra_subject = entry.get("entra_subject")?.as_str()?.trim().to_string();
            let bridge_subject = entry.get("bridge_subject")?.as_str()?.trim().to_string();
            let name = entry.get("name")?.as_str()?.trim().to_string();
            let roles: Vec<String> = entry
                .get("roles")?
                .as_array()?
                .iter()
                .filter_map(|r| r.as_str().map(|s| s.trim().to_string()))
                .filter(|r| !r.is_empty())
                .collect();
            if entra_subject.is_empty()
                || bridge_subject.is_empty()
                || name.is_empty()
                || roles.is_empty()
            {
                return None;
            }
            Some((entra_subject, bridge_subject, roles, name))
        })
        .collect()
}
