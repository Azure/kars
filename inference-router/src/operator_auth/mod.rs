// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Operator-tier authorization for router endpoints that change or reveal
//! sandbox security controls (egress learn mode, the effective allowlist,
//! the audit chain, rate limits and registry reputation).
//!
//! The agent container shares the pod network namespace with the router,
//! so neither a loopback peer address nor the pod IP identifies an
//! operator, and the admin token is readable from the agent container.
//! For same-pod callers this middleware therefore attributes the TCP
//! connection to the Linux UID that owns the client socket (from
//! `/proc/net/tcp` and `/proc/net/tcp6`) and admits only:
//!
//! * the router's own UID — `kars-inference-router probe` run through
//!   `kubectl exec -c inference-router`, and
//! * UID 0 — `kubectl port-forward` and `docker exec` (dev mode).
//!
//! Every other same-pod UID, including the agent, is refused even when it
//! presents a valid admin token. Connections the router process opened to
//! itself (for example through `/egress/fetch`) are refused as well.
//! Attribution failures of any kind are refused (fail closed).
//!
//! Callers outside the pod keep the existing admin-API semantics: optional
//! `ROUTER_ADMIN_ALLOW_IPS` origin allowlist, then
//! `Authorization: Bearer <admin-token>`.

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// Kernel TCP state code for `TCP_ESTABLISHED` in `/proc/net/tcp`.
const TCP_ESTABLISHED: u8 = 0x01;

/// Response body for operator endpoints refused to same-pod workloads.
pub const OPERATOR_DENIED_BODY: &str = "Operator endpoint not available to sandbox workloads";

/// One row of `/proc/net/tcp` or `/proc/net/tcp6`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpSocketEntry {
    pub local: SocketAddr,
    pub remote: SocketAddr,
    pub state: u8,
    pub uid: u32,
    pub inode: u64,
}

/// Why a same-pod caller was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// The socket table could not be read.
    SnapshotUnavailable,
    /// No client socket matched the peer address.
    SocketNotFound,
    /// A matching socket was not ESTABLISHED or had no owning inode.
    SocketNotEstablished,
    /// Matching sockets reported different owners.
    AmbiguousOwner,
    /// The connection was opened by the router process itself.
    SelfOriginated,
    /// The socket is owned by a UID that is not an operator.
    UntrustedUid(u32),
}

impl DenyReason {
    fn as_str(&self) -> &'static str {
        match self {
            Self::SnapshotUnavailable => "snapshot_unavailable",
            Self::SocketNotFound => "socket_not_found",
            Self::SocketNotEstablished => "socket_not_established",
            Self::AmbiguousOwner => "ambiguous_owner",
            Self::SelfOriginated => "self_originated",
            Self::UntrustedUid(_) => "untrusted_uid",
        }
    }
}

/// Outcome of attributing a same-pod connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalPeerDecision {
    Allow { uid: u32 },
    Deny(DenyReason),
}

/// How the middleware classified the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PeerClass {
    Remote,
    Local(LocalPeerDecision),
}

/// Map an IPv4-mapped IPv6 address to its IPv4 form.
fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

fn normalize_socket_addr(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(IpAddr::V4(v4), v6.port()),
            None => addr,
        },
        v4 => v4,
    }
}

/// Parse one `ADDR:PORT` column. The kernel prints each 32-bit address
/// word as a native-endian integer in hex and the port in host order.
fn parse_hex_socket_addr(field: &str) -> Option<SocketAddr> {
    let (addr_hex, port_hex) = field.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    let ip = match addr_hex.len() {
        8 => {
            let word = u32::from_str_radix(addr_hex, 16).ok()?;
            IpAddr::V4(Ipv4Addr::from(word.to_ne_bytes()))
        }
        32 => {
            let mut bytes = [0u8; 16];
            for (i, chunk) in bytes.chunks_exact_mut(4).enumerate() {
                let word = u32::from_str_radix(addr_hex.get(i * 8..i * 8 + 8)?, 16).ok()?;
                chunk.copy_from_slice(&word.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(normalize_ip(ip), port))
}

/// Parse the contents of `/proc/net/tcp` or `/proc/net/tcp6`. The header
/// and malformed rows are skipped; a skipped row can only cause a later
/// "socket not found" refusal, never an approval.
pub fn parse_proc_net_tcp(contents: &str) -> Vec<TcpSocketEntry> {
    contents
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 10 || !fields[0].ends_with(':') {
                return None;
            }
            Some(TcpSocketEntry {
                local: parse_hex_socket_addr(fields[1])?,
                remote: parse_hex_socket_addr(fields[2])?,
                state: u8::from_str_radix(fields[3], 16).ok()?,
                uid: fields[7].parse().ok()?,
                inode: fields[9].parse().ok()?,
            })
        })
        .collect()
}

/// Read and parse the pod's TCP socket tables. `/proc/net/tcp6` may be
/// absent when IPv6 is disabled; `/proc/net/tcp` must be readable.
pub fn read_tcp_sockets() -> io::Result<Vec<TcpSocketEntry>> {
    let mut sockets = parse_proc_net_tcp(&std::fs::read_to_string("/proc/net/tcp")?);
    match std::fs::read_to_string("/proc/net/tcp6") {
        Ok(contents) => sockets.extend(parse_proc_net_tcp(&contents)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    Ok(sockets)
}

/// Extract the inode from a `/proc/<pid>/fd` link target of the form
/// `socket:[12345]`.
pub fn parse_socket_link(target: &str) -> Option<u64> {
    target
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// Inodes of every socket currently open in this process.
pub fn read_self_socket_inodes() -> io::Result<HashSet<u64>> {
    let mut inodes = HashSet::new();
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        // A descriptor closed between listing and readlink is no longer
        // ours, so skipping it is correct.
        if let Ok(target) = std::fs::read_link(entry.path())
            && let Some(inode) = target.to_str().and_then(parse_socket_link)
        {
            inodes.insert(inode);
        }
    }
    Ok(inodes)
}

/// Extract the filesystem UID (the UID the kernel records as a new
/// socket's owner) from `/proc/self/status`.
pub fn parse_status_fsuid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()
}

/// This process's filesystem UID, or `None` when `/proc` is unavailable.
pub fn read_self_fsuid() -> Option<u32> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .as_deref()
        .and_then(parse_status_fsuid)
}

/// Whether `peer` is an address of this network namespace (loopback or an
/// address assigned to a local interface such as the pod IP). An address
/// can be bound only when it is local; `AddrNotAvailable` means remote.
/// Any other error is treated as local so the caller must pass socket
/// attribution (fail closed).
pub fn is_local_address(peer: SocketAddr) -> bool {
    let mut probe = normalize_socket_addr(peer);
    if probe.ip().is_loopback() || probe.ip().is_unspecified() {
        return true;
    }
    probe.set_port(0);
    match UdpSocket::bind(probe) {
        Ok(_) => true,
        Err(err) => err.kind() != io::ErrorKind::AddrNotAvailable,
    }
}

/// Attribute a same-pod connection from `peer` to the router listening on
/// `server_port` and decide whether the caller is an operator.
pub fn attribute_local_peer(
    sockets: &[TcpSocketEntry],
    peer: SocketAddr,
    server_port: u16,
    self_inodes: &HashSet<u64>,
    router_uid: Option<u32>,
) -> LocalPeerDecision {
    let peer = normalize_socket_addr(peer);
    let candidates: Vec<&TcpSocketEntry> = sockets
        .iter()
        .filter(|s| s.local == peer && s.remote.port() == server_port)
        .collect();

    let Some(first) = candidates.first() else {
        return LocalPeerDecision::Deny(DenyReason::SocketNotFound);
    };
    if candidates
        .iter()
        .any(|s| s.state != TCP_ESTABLISHED || s.inode == 0)
    {
        return LocalPeerDecision::Deny(DenyReason::SocketNotEstablished);
    }
    if candidates.iter().any(|s| s.uid != first.uid) {
        return LocalPeerDecision::Deny(DenyReason::AmbiguousOwner);
    }
    if candidates.iter().any(|s| self_inodes.contains(&s.inode)) {
        return LocalPeerDecision::Deny(DenyReason::SelfOriginated);
    }
    let uid = first.uid;
    if uid == 0 || Some(uid) == router_uid {
        LocalPeerDecision::Allow { uid }
    } else {
        LocalPeerDecision::Deny(DenyReason::UntrustedUid(uid))
    }
}

/// Admin-API check for callers outside the pod: optional origin allowlist,
/// then a constant-time bearer-token comparison.
pub async fn check_remote_admin(
    expected_token: &str,
    allow_ips: &[IpAddr],
    remote_ip: Option<IpAddr>,
    req: Request,
    next: Next,
) -> Response {
    // s3 — origin allowlist (defence in depth against admin-token theft).
    if !allow_ips.is_empty() {
        match remote_ip {
            Some(ip) if allow_ips.contains(&ip) => {}
            Some(ip) => {
                tracing::warn!(
                    path = %req.uri().path(),
                    remote = %ip,
                    "Admin auth: remote IP not in ROUTER_ADMIN_ALLOW_IPS"
                );
                return (StatusCode::FORBIDDEN, "Admin origin not allowed").into_response();
            }
            None => {
                tracing::warn!(
                    path = %req.uri().path(),
                    "Admin auth: missing ConnectInfo while allowlist is set"
                );
                return (StatusCode::FORBIDDEN, "Admin origin not allowed").into_response();
            }
        }
    }

    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(value) if value.starts_with("Bearer ") => {
            let provided = &value[7..];
            if crate::handoff::constant_time_eq(provided.as_bytes(), expected_token.as_bytes()) {
                next.run(req).await.into_response()
            } else {
                tracing::warn!(
                    path = %req.uri().path(),
                    "Admin auth: invalid token from non-localhost"
                );
                (StatusCode::UNAUTHORIZED, "Invalid admin token").into_response()
            }
        }
        _ => {
            tracing::warn!(
                path = %req.uri().path(),
                "Admin auth: non-localhost request without token"
            );
            (
                StatusCode::UNAUTHORIZED,
                "Admin token required for non-localhost access",
            )
                .into_response()
        }
    }
}

/// Configuration for [`operator_auth_middleware`]. Deliberately not
/// `Debug`: it holds the admin token.
#[derive(Clone)]
pub struct OperatorAuth {
    admin_token: Arc<String>,
    allow_ips: Arc<Vec<IpAddr>>,
    server_port: u16,
    router_uid: Option<u32>,
}

impl OperatorAuth {
    /// Build the gate for a router listening on `server_port`, using this
    /// process's filesystem UID as the router UID.
    pub fn new(admin_token: Arc<String>, allow_ips: Arc<Vec<IpAddr>>, server_port: u16) -> Self {
        Self::with_router_uid(admin_token, allow_ips, server_port, read_self_fsuid())
    }

    pub fn with_router_uid(
        admin_token: Arc<String>,
        allow_ips: Arc<Vec<IpAddr>>,
        server_port: u16,
        router_uid: Option<u32>,
    ) -> Self {
        Self {
            admin_token,
            allow_ips,
            server_port,
            router_uid,
        }
    }

    fn classify(&self, peer: SocketAddr) -> PeerClass {
        if !is_local_address(peer) {
            return PeerClass::Remote;
        }
        let decision = match (read_self_socket_inodes(), read_tcp_sockets()) {
            (Ok(self_inodes), Ok(sockets)) => attribute_local_peer(
                &sockets,
                peer,
                self.server_port,
                &self_inodes,
                self.router_uid,
            ),
            _ => LocalPeerDecision::Deny(DenyReason::SnapshotUnavailable),
        };
        PeerClass::Local(decision)
    }
}

/// Gate operator-tier endpoints. See the module documentation.
pub async fn operator_auth_middleware(
    auth: Arc<OperatorAuth>,
    req: Request,
    next: Next,
) -> Response {
    let Some(peer) = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)
    else {
        return check_remote_admin(&auth.admin_token, &auth.allow_ips, None, req, next).await;
    };

    let classifier = auth.clone();
    let class = tokio::task::spawn_blocking(move || classifier.classify(peer))
        .await
        .unwrap_or(PeerClass::Local(LocalPeerDecision::Deny(
            DenyReason::SnapshotUnavailable,
        )));

    match class {
        PeerClass::Remote => {
            check_remote_admin(
                &auth.admin_token,
                &auth.allow_ips,
                Some(peer.ip()),
                req,
                next,
            )
            .await
        }
        PeerClass::Local(LocalPeerDecision::Allow { .. }) => next.run(req).await.into_response(),
        PeerClass::Local(LocalPeerDecision::Deny(reason)) => {
            let uid = match reason {
                DenyReason::UntrustedUid(uid) => Some(uid),
                _ => None,
            };
            tracing::warn!(
                path = %req.uri().path(),
                reason = reason.as_str(),
                uid = uid,
                "Operator auth: refused same-pod caller"
            );
            (StatusCode::FORBIDDEN, OPERATOR_DENIED_BODY).into_response()
        }
    }
}

#[cfg(test)]
mod tests;
