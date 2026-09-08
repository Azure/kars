// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Scope-fenced capability requests. Decisions are observations, never grants.

use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub mod scope;
pub use scope::{Identity, Scope};

#[cfg(test)]
#[path = "access_request/dispatch_tests.rs"]
mod dispatch_tests;

const CAPACITY: usize = 64;
const REQUESTS_PER_MINUTE: u32 = 32;
const TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Approved,
    Denied,
    Cancelled,
    Expired,
    DispatchClaimed,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub scope_id: String,
    pub kind: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub reason: String,
    pub tier: Option<u8>,
    pub port: Option<u16>,
}

#[derive(Clone, Serialize)]
pub struct Entry {
    pub request_id: String,
    pub scope_id: String,
    pub kind: String,
    pub target: String,
    pub reason: String,
    pub tier: Option<u8>,
    pub port: Option<u16>,
    pub status: Status,
    pub decision: Option<Status>,
    pub count: u32,
    pub first_seen_unix: u64,
    pub expires_at_unix: u64,
    pub dispatch_active: bool,
    #[serde(skip)]
    expires: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    StaleScope,
    Full,
    RateLimited,
    Missing,
    Expired,
    WaitTimeout,
    Terminal,
    Unavailable,
}

struct Inner {
    scope: Scope,
    generation: u64,
    sequence: u64,
    requests: VecDeque<Entry>,
    window: Instant,
    rate: u32,
    cancellation: CancellationToken,
    active_dispatches: usize,
}

pub struct AccessRequestBuffer {
    inner: Mutex<Inner>,
    changed: watch::Sender<u64>,
    capacity: usize,
    rate_limit: u32,
    ttl: Duration,
}

/// The linearization boundary for an outgoing operation, not evidence that an
/// upstream accepted it. Cancel/reset cannot acknowledge prevention while held.
#[must_use]
pub struct DispatchClaim<'a> {
    buffer: &'a AccessRequestBuffer,
    request_id: Option<String>,
}

impl Drop for DispatchClaim<'_> {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.buffer.inner.lock() {
            inner.active_dispatches = inner.active_dispatches.saturating_sub(1);
            if let Some(id) = &self.request_id
                && let Some(entry) = inner
                    .requests
                    .iter_mut()
                    .find(|entry| &entry.request_id == id)
            {
                entry.dispatch_active = false;
            }
        }
        self.buffer.notify();
    }
}

impl AccessRequestBuffer {
    pub fn new(identity: Identity) -> Self {
        Self::with_limits(identity, CAPACITY, REQUESTS_PER_MINUTE, TTL)
    }

    pub fn with_limits(
        identity: Identity,
        capacity: usize,
        rate_limit: u32,
        ttl: Duration,
    ) -> Self {
        let scope = Scope {
            id: format!("{:032x}:0", rand::random::<u128>()),
            identity,
            assignment_id: None,
        };
        Self {
            inner: Mutex::new(Inner {
                scope,
                generation: 0,
                sequence: 0,
                requests: VecDeque::new(),
                window: Instant::now(),
                rate: 0,
                cancellation: CancellationToken::new(),
                active_dispatches: 0,
            }),
            changed: watch::channel(0).0,
            capacity: capacity.clamp(1, 256),
            rate_limit: rate_limit.clamp(1, 128),
            ttl: ttl.min(Duration::from_secs(3600)),
        }
    }

    fn notify(&self) {
        self.changed
            .send_modify(|value| *value = value.saturating_add(1));
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    pub fn scope(&self) -> Result<Scope, Error> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| Error::Unavailable)?
            .scope
            .clone())
    }

    fn check(inner: &Inner, scope: &str) -> Result<(), Error> {
        if inner.scope.id == scope {
            Ok(())
        } else {
            Err(Error::StaleScope)
        }
    }
    fn expire(inner: &mut Inner) {
        for entry in &mut inner.requests {
            if entry.expires <= Instant::now()
                && matches!(entry.status, Status::Pending | Status::Approved)
            {
                entry.status = Status::Expired;
            }
        }
    }

    pub fn record(&self, mut request: Request) -> Result<(Entry, bool), Error> {
        validate(&mut request)?;
        let mut inner = self.inner.lock().map_err(|_| Error::Unavailable)?;
        Self::check(&inner, &request.scope_id)?;
        if inner.window.elapsed() >= Duration::from_secs(60) {
            inner.window = Instant::now();
            inner.rate = 0;
        }
        if inner.rate >= self.rate_limit {
            return Err(Error::RateLimited);
        }
        inner.rate += 1;
        Self::expire(&mut inner);
        if let Some(entry) = inner.requests.iter_mut().find(|entry| {
            entry.kind == request.kind
                && entry.target == request.target
                && entry.port == request.port
                && entry.tier == request.tier
                && entry.expires > Instant::now()
        }) {
            // An agent cannot rewrite the reviewed payload or extend its lifetime.
            entry.count = entry.count.saturating_add(1);
            return Ok((entry.clone(), false));
        }
        if inner.requests.len() >= self.capacity {
            if let Some(index) = inner
                .requests
                .iter()
                .position(|entry| entry.status != Status::Pending && !entry.dispatch_active)
            {
                inner.requests.remove(index);
            } else {
                return Err(Error::Full);
            }
        }
        inner.sequence = inner.sequence.checked_add(1).ok_or(Error::Unavailable)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let entry = Entry {
            request_id: format!("{}:{}", inner.scope.id, inner.sequence),
            scope_id: inner.scope.id.clone(),
            kind: request.kind,
            target: request.target,
            reason: request.reason,
            tier: request.tier,
            port: request.port,
            status: Status::Pending,
            decision: None,
            count: 1,
            first_seen_unix: now,
            expires_at_unix: now.saturating_add(self.ttl.as_secs()),
            dispatch_active: false,
            expires: Instant::now() + self.ttl,
        };
        inner.requests.push_back(entry.clone());
        drop(inner);
        self.notify();
        Ok((entry, true))
    }

    pub fn snapshot(&self, scope: &str) -> Result<Vec<Entry>, Error> {
        let mut inner = self.inner.lock().map_err(|_| Error::Unavailable)?;
        Self::check(&inner, scope)?;
        Self::expire(&mut inner);
        Ok(inner.requests.iter().cloned().collect())
    }

    pub fn transition(&self, scope: &str, id: &str, status: Status) -> Result<Entry, Error> {
        if !matches!(
            status,
            Status::Approved | Status::Denied | Status::Cancelled
        ) {
            return Err(Error::Invalid);
        }
        let mut inner = self.inner.lock().map_err(|_| Error::Unavailable)?;
        Self::check(&inner, scope)?;
        Self::expire(&mut inner);
        let entry = inner
            .requests
            .iter_mut()
            .find(|entry| entry.request_id == id)
            .ok_or(Error::Missing)?;
        if entry.status == Status::DispatchClaimed {
            return Err(Error::Terminal);
        }
        if entry.expires <= Instant::now() {
            return Err(Error::Expired);
        }
        if entry.status != Status::Pending
            && !(status == Status::Cancelled && entry.status == Status::Approved)
        {
            return Err(Error::Terminal);
        }
        if matches!(status, Status::Approved | Status::Denied) {
            entry.decision = Some(status);
        }
        entry.status = status;
        let entry = entry.clone();
        drop(inner);
        self.notify();
        Ok(entry)
    }

    pub fn reset(
        &self,
        expected_scope: &str,
        assignment_id: Option<String>,
    ) -> Result<(Scope, usize), Error> {
        if assignment_id
            .as_deref()
            .is_some_and(|id| !scope::identifier(id, 253))
        {
            return Err(Error::Invalid);
        }
        let mut inner = self.inner.lock().map_err(|_| Error::Unavailable)?;
        Self::check(&inner, expected_scope)?;
        if inner.active_dispatches > 0 {
            return Err(Error::Terminal);
        }
        inner.generation = inner.generation.checked_add(1).ok_or(Error::Unavailable)?;
        let instance = inner.scope.id.split(':').next().ok_or(Error::Unavailable)?;
        inner.scope.id = format!("{instance}:{}", inner.generation);
        inner.scope.assignment_id = assignment_id;
        let cleared = inner.requests.len();
        inner.requests.clear();
        inner.cancellation.cancel();
        inner.cancellation = CancellationToken::new();
        let scope = inner.scope.clone();
        drop(inner);
        self.notify();
        Ok((scope, cleared))
    }

    pub fn cancellation(&self, scope: &str) -> Result<CancellationToken, Error> {
        let inner = self.inner.lock().map_err(|_| Error::Unavailable)?;
        Self::check(&inner, scope)?;
        Ok(inner.cancellation.clone())
    }

    pub fn entry(&self, scope: &str, id: &str) -> Result<Entry, Error> {
        self.snapshot(scope)?
            .into_iter()
            .find(|entry| entry.request_id == id)
            .ok_or(Error::Missing)
    }

    fn dispatch_ready(
        inner: &mut Inner,
        scope: &str,
        id: Option<&str>,
        shutdown: &CancellationToken,
    ) -> Result<(), Error> {
        Self::check(inner, scope)?;
        if shutdown.is_cancelled() {
            return Err(Error::Unavailable);
        }
        Self::expire(inner);
        if let Some(id) = id {
            let entry = inner
                .requests
                .iter()
                .find(|entry| entry.request_id == id)
                .ok_or(Error::Missing)?;
            if entry.status == Status::Expired {
                return Err(Error::Expired);
            }
            if entry.status != Status::Approved {
                return Err(Error::Terminal);
            }
        }
        Ok(())
    }

    /// Recheck after an asynchronous policy lookup. This is deliberately not a
    /// dispatch permit: callers must still claim at the actual send boundary.
    pub fn validate_dispatch(
        &self,
        scope: &str,
        id: &str,
        shutdown: &CancellationToken,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().map_err(|_| Error::Unavailable)?;
        Self::dispatch_ready(&mut inner, scope, Some(id), shutdown)
    }

    pub fn claim_dispatch(
        &self,
        scope: &str,
        id: Option<&str>,
        shutdown: &CancellationToken,
    ) -> Result<DispatchClaim<'_>, Error> {
        let mut inner = self.inner.lock().map_err(|_| Error::Unavailable)?;
        Self::dispatch_ready(&mut inner, scope, id, shutdown)?;
        let active = inner.active_dispatches.checked_add(1).ok_or(Error::Full)?;
        if let Some(id) = id {
            let entry = inner
                .requests
                .iter_mut()
                .find(|entry| entry.request_id == id)
                .ok_or(Error::Missing)?;
            entry.status = Status::DispatchClaimed;
            entry.dispatch_active = true;
        }
        inner.active_dispatches = active;
        drop(inner);
        self.notify();
        Ok(DispatchClaim {
            buffer: self,
            request_id: id.map(str::to_string),
        })
    }
}

fn validate(request: &mut Request) -> Result<(), Error> {
    request.kind = request.kind.trim().to_ascii_lowercase();
    request.target = request.target.trim().to_string();
    if ![
        "egress",
        "tool",
        "skill",
        "mcp",
        "command",
        "permission",
        "clarification",
        "tier",
    ]
    .contains(&request.kind.as_str())
        || request.reason.len() > 512
        || request.reason.chars().any(|c| c.is_control() && c != '\n')
    {
        return Err(Error::Invalid);
    }
    if request.kind == "tier" {
        if !request.tier.is_some_and(|tier| (1..=5).contains(&tier))
            || !request.target.is_empty()
            || request.port.is_some()
        {
            return Err(Error::Invalid);
        }
    } else {
        if !scope::identifier(&request.target, 253) || request.tier.is_some() {
            return Err(Error::Invalid);
        }
        if request.kind == "egress" {
            if request.target.contains(['/', ':']) {
                return Err(Error::Invalid);
            }
            request.target =
                crate::egress_blocked::normalize_host(&request.target).ok_or(Error::Invalid)?;
            request.port = Some(request.port.unwrap_or(443));
            if request.port == Some(0) {
                return Err(Error::Invalid);
            }
        } else if request.port.is_some() {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}
