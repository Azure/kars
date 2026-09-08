// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    access_request::{AccessRequestBuffer, Error, Identity, Request, Scope, Status},
    blocklist::Blocklist,
    task_telemetry::TaskTelemetry,
};
use std::{net::IpAddr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub const CONTROL_TOKEN_PATH: &str = "/etc/kars/services/control-token";
pub const MAX_WAIT: Duration = Duration::from_secs(300);
const WAIT_CAPACITY: usize = 16;

pub struct GovernedServices {
    pub requests: AccessRequestBuffer,
    pub telemetry: Arc<TaskTelemetry>,
    control_token: Option<String>,
    pub observer: Option<Arc<crate::service_observation::Observer>>,
    pub allow_ips: Option<Vec<IpAddr>>,
    pub identity_valid: bool,
    pub shutdown: CancellationToken,
    waits: Arc<Semaphore>,
    reset_gate: std::sync::Mutex<()>,
}

impl Default for GovernedServices {
    fn default() -> Self {
        Self::new(Identity::standalone("unknown"), None)
    }
}

impl GovernedServices {
    pub fn new(identity: Identity, control_token: Option<String>) -> Self {
        let control_token = control_token.filter(|token| {
            (32..=256).contains(&token.len())
                && token.is_ascii()
                && !token.chars().any(|c| c.is_whitespace() || c.is_control())
        });
        let requests = AccessRequestBuffer::new(identity);
        let scope = requests
            .scope()
            .expect("fresh service state is not poisoned");
        Self {
            requests,
            telemetry: Arc::new(TaskTelemetry::new(scope.id)),
            control_token,
            observer: None,
            allow_ips: None,
            identity_valid: true,
            shutdown: CancellationToken::new(),
            waits: Arc::new(Semaphore::new(WAIT_CAPACITY)),
            reset_gate: std::sync::Mutex::new(()),
        }
    }
    pub fn from_env(sandbox: &str) -> Self {
        let identity = std::env::var("KARS_SERVICE_IDENTITY_JSON")
            .ok()
            .map(|raw| serde_json::from_str::<Identity>(&raw).ok());
        let valid = identity.as_ref().is_none_or(|identity| {
            identity
                .as_ref()
                .is_some_and(|identity| identity.valid(sandbox))
        });
        let identity = identity
            .flatten()
            .unwrap_or_else(|| Identity::standalone(sandbox));
        let token = std::env::var("KARS_SERVICES_ADMIN_TOKEN")
            .ok()
            .or_else(|| std::fs::read_to_string(CONTROL_TOKEN_PATH).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let mut services = Self::new(identity, token);
        match crate::service_observation::Observer::load() {
            Ok(observer) => services.observer = observer,
            Err(_) => tracing::warn!(
                "Private observation configuration is unavailable; observation routes fail closed"
            ),
        }
        services.identity_valid = valid;
        services.allow_ips = std::env::var("ROUTER_ADMIN_ALLOW_IPS")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .map(str::parse::<IpAddr>)
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap_or_default()
            });
        services
    }
    pub fn control_authorized(&self, provided: Option<&str>) -> bool {
        self.identity_valid
            && self
                .control_token
                .as_deref()
                .zip(provided)
                .is_some_and(|(expected, provided)| {
                    crate::handoff::constant_time_eq(expected.as_bytes(), provided.as_bytes())
                })
    }
    pub fn control_configured(&self) -> bool {
        self.identity_valid && self.control_token.is_some()
    }
    pub fn reset(
        &self,
        expected: &str,
        assignment: Option<String>,
    ) -> Result<(Scope, usize), Error> {
        let _guard = self.reset_gate.lock().map_err(|_| Error::Unavailable)?;
        let result = self.requests.reset(expected, assignment)?;
        self.telemetry.reset(result.0.id.clone());
        Ok(result)
    }
    pub fn record_egress_in_scope(
        &self,
        scope: &str,
        host: &str,
        port: u16,
    ) -> Result<crate::access_request::Entry, Error> {
        self.requests
            .record(Request {
                scope_id: scope.into(),
                kind: "egress".into(),
                target: host.into(),
                reason: "Egress policy blocked this destination".into(),
                tier: None,
                port: Some(port),
            })
            .map(|(entry, _)| entry)
    }
    pub fn wait_slots(&self) -> usize {
        self.waits.available_permits()
    }

    pub async fn wait_for_decision(
        &self,
        scope: &str,
        id: &str,
        timeout: Duration,
    ) -> Result<crate::access_request::Entry, Error> {
        let _permit = self
            .waits
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Full)?;
        let cancellation = self.requests.cancellation(scope)?;
        let mut changed = self.requests.subscribe();
        let future = async {
            loop {
                let entry = self.requests.entry(scope, id)?;
                if entry.status != Status::Pending {
                    return Ok(entry);
                }
                tokio::select! {
                    _ = cancellation.cancelled() => return Err(Error::StaleScope),
                    _ = self.shutdown.cancelled() => return Err(Error::Unavailable),
                    _ = changed.changed() => {},
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
        };
        tokio::time::timeout(timeout.min(MAX_WAIT), future)
            .await
            .map_err(|_| Error::WaitTimeout)?
    }

    pub async fn wait_for_egress(
        &self,
        blocklist: &Blocklist,
        scope: &str,
        id: &str,
        target: &str,
        sandbox: &str,
        timeout: Duration,
    ) -> Result<(), Error> {
        self.wait_for_egress_check(scope, id, target, sandbox, timeout, || {
            blocklist.check_egress(target, sandbox)
        })
        .await
    }

    pub(crate) async fn wait_for_egress_check<F, Fut>(
        &self,
        scope: &str,
        id: &str,
        target: &str,
        sandbox: &str,
        timeout: Duration,
        mut check_policy: F,
    ) -> Result<(), Error>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let url = reqwest::Url::parse(target).map_err(|_| Error::Invalid)?;
        let host = url.host_str().ok_or(Error::Invalid)?;
        let port = url.port_or_known_default().ok_or(Error::Invalid)?;
        let owner = self.requests.scope()?;
        if owner.id != scope || owner.identity.sandbox.name != sandbox {
            return Err(Error::StaleScope);
        }
        let _permit = self
            .waits
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Full)?;
        let cancellation = self.requests.cancellation(scope)?;
        let mut changed = self.requests.subscribe();
        let future = async {
            loop {
                let entry = self.requests.entry(scope, id)?;
                if entry.kind != "egress"
                    || !entry.target.eq_ignore_ascii_case(host)
                    || entry.port != Some(port)
                {
                    return Err(Error::Invalid);
                }
                match entry.status {
                    Status::Denied | Status::Cancelled | Status::DispatchClaimed => {
                        return Err(Error::Terminal);
                    }
                    Status::Expired => return Err(Error::Expired),
                    Status::Approved => {
                        // The decision API never changes enforcement. Only the
                        // normal signed-policy path can make this check pass.
                        let allowed = tokio::select! {
                            result = check_policy() => result.is_ok(),
                            _ = cancellation.cancelled() => return Err(Error::StaleScope),
                            _ = self.shutdown.cancelled() => return Err(Error::Unavailable),
                            _ = changed.changed() => continue,
                        };
                        if allowed {
                            self.requests.validate_dispatch(scope, id, &self.shutdown)?;
                            return Ok(());
                        }
                    }
                    Status::Pending => {}
                }
                tokio::select! {
                    _ = cancellation.cancelled() => return Err(Error::StaleScope),
                    _ = self.shutdown.cancelled() => return Err(Error::Unavailable),
                    _ = changed.changed() => {},
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
        };
        tokio::time::timeout(timeout.min(MAX_WAIT), future)
            .await
            .map_err(|_| Error::WaitTimeout)?
    }

    /// Claim immediately before creating/polling the outgoing send future.
    /// This shares the request mutex with cancellation and scope reset.
    pub fn claim_egress_dispatch(
        &self,
        scope: &str,
        request_id: Option<&str>,
    ) -> Result<crate::access_request::DispatchClaim<'_>, Error> {
        self.requests
            .claim_dispatch(scope, request_id, &self.shutdown)
    }
}
