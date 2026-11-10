// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::Blocklist;
use crate::governed_services::GovernedServices;
use std::sync::Arc;

#[cfg(test)]
#[path = "scoped_grants_tests.rs"]
mod tests;

#[derive(Clone, Debug)]
pub(crate) struct ScopedEgressGrant {
    pub scope_id: String,
    pub request_id: String,
    pub endpoints: Vec<(String, u16)>,
    pub expires_at: i64,
}

impl ScopedEgressGrant {
    pub(crate) fn from_document(doc: &serde_json::Value) -> Option<Self> {
        if doc.get("schemaVersion")?.as_u64()? != 2 || doc.get("hosts").is_some() {
            return None;
        }
        let scope_id = doc.get("requestScope")?.as_str()?;
        let request_id = doc.get("requestId")?.as_str()?;
        let (nonce, generation) = scope_id.split_once(':')?;
        if nonce.len() != 32
            || !nonce.bytes().all(|b| b.is_ascii_hexdigit())
            || generation.parse::<u64>().is_err()
            || scope_id.len() > 64
            || request_id.len() > 96
        {
            return None;
        }
        if request_id
            .strip_prefix(&format!("{scope_id}:"))?
            .parse::<u64>()
            .ok()?
            == 0
        {
            return None;
        }
        let effective_at = chrono::DateTime::parse_from_rfc3339(doc.get("effectiveAt")?.as_str()?)
            .ok()?
            .timestamp();
        let expires_at = chrono::DateTime::parse_from_rfc3339(doc.get("expiresAt")?.as_str()?)
            .ok()?
            .timestamp();
        let now = chrono::Utc::now().timestamp();
        if effective_at > now || expires_at <= now || expires_at <= effective_at {
            return None;
        }
        let hosts = doc.get("scopedHosts")?.as_array()?;
        if hosts.len() != 1 {
            return None;
        }
        let host = hosts[0].get("host")?.as_str()?;
        if crate::egress_blocked::normalize_host(host).as_deref() != Some(host) {
            return None;
        }
        let port = u16::try_from(hosts[0].get("port")?.as_u64()?).ok()?;
        if port == 0 {
            return None;
        }
        Some(Self {
            scope_id: scope_id.into(),
            request_id: request_id.into(),
            endpoints: vec![(host.into(), port)],
            expires_at,
        })
    }

    fn permits(&self, services: &GovernedServices, host: &str, port: u16) -> bool {
        services.identity_valid
            && !services.shutdown.is_cancelled()
            && self.expires_at > chrono::Utc::now().timestamp()
            && self.endpoints.iter().any(|(h, p)| h == host && *p == port)
            && services.requests.authorizes_egress_grant(
                &self.scope_id,
                &self.request_id,
                host,
                port,
            )
    }
}

impl Blocklist {
    pub fn bind_governed_services(&self, services: Arc<GovernedServices>) -> anyhow::Result<()> {
        self.services
            .set(services)
            .map_err(|_| anyhow::anyhow!("egress service scope already bound"))
    }

    pub(crate) fn applicable_grant_endpoints(
        &self,
        grants: &[ScopedEgressGrant],
    ) -> Vec<(String, u16)> {
        let Some(services) = self.services.get() else {
            return Vec::new();
        };
        grants
            .iter()
            .flat_map(|grant| {
                grant
                    .endpoints
                    .iter()
                    .filter(|(host, port)| grant.permits(services, host, *port))
                    .cloned()
            })
            .collect()
    }

    pub(super) async fn scoped_egress_request(&self, url: &str, scope: &str) -> Option<String> {
        let services = self.services.get()?;
        let parsed = reqwest::Url::parse(url).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return None;
        }
        let host = parsed.host_str()?;
        let port = parsed.port_or_known_default()?;
        self.scoped_grants
            .read()
            .await
            .iter()
            .find(|grant| grant.scope_id == scope && grant.permits(services, host, port))
            .map(|grant| grant.request_id.clone())
    }
}
