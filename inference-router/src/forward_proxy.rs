// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Transparent HTTP forward proxy for egress enforcement.
//!
//! Listens on a separate port (default 8444) and handles:
//!   - HTTP CONNECT (HTTPS tunneling): extract domain, check blocklist, then tunnel
//!   - Plain HTTP proxy: extract Host header, check blocklist, then proxy
//!
//! iptables REDIRECT sends all outbound TCP 80/443 from UID 1000 to this port,
//! making the proxy completely transparent to applications in the sandbox.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::blocklist::Blocklist;
use crate::egress_blocked::BlockedBuffer;

/// Maximum concurrent tunnel connections (prevents resource exhaustion).
const MAX_CONCURRENT_TUNNELS: usize = 256;

/// Maximum lifetime for a single tunnel (1 hour hard cap).
const MAX_TUNNEL_LIFETIME_SECS: u64 = 3600;

/// Start the transparent forward proxy on the given address.
/// Returns a CancellationToken that triggers graceful shutdown when cancelled.
pub async fn start(
    addr: &str,
    blocklist: Blocklist,
    blocked_egress: Arc<BlockedBuffer>,
) -> CancellationToken {
    let shutdown = CancellationToken::new();
    let shutdown_clone = shutdown.clone();

    let addr = addr.to_string();
    tokio::spawn(async move {
        run(addr, blocklist, blocked_egress, shutdown_clone).await;
    });

    shutdown
}

async fn run(
    addr: String,
    blocklist: Blocklist,
    blocked_egress: Arc<BlockedBuffer>,
    shutdown: CancellationToken,
) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, addr = %addr, "Forward proxy failed to bind");
            return;
        }
    };

    let sandbox_name = std::env::var("SANDBOX_NAME").unwrap_or_else(|_| "unknown".into());
    tracing::info!(addr = %addr, sandbox = %sandbox_name, "Forward proxy listening (transparent egress)");
    let blocklist = Arc::new(blocklist);
    let sandbox_name = Arc::new(sandbox_name);
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_TUNNELS));
    let active_count = Arc::new(AtomicUsize::new(0));

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer) = match result {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "Forward proxy accept error");
                        continue;
                    }
                };

                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::warn!(peer = %peer, active = active_count.load(Ordering::Relaxed),
                            "Forward proxy: connection limit reached, rejecting");
                        drop(stream);
                        continue;
                    }
                };

                let bl = blocklist.clone();
                let sb = sandbox_name.clone();
                let be = blocked_egress.clone();
                let count = active_count.clone();
                let conn_shutdown = shutdown.clone();
                count.fetch_add(1, Ordering::Relaxed);
                tokio::spawn(async move {
                    tokio::select! {
                        result = handle_connection(stream, &bl, &sb, &be) => {
                            if let Err(e) = result {
                                tracing::debug!(peer = %peer, error = %e, "Forward proxy connection error");
                            }
                        }
                        _ = conn_shutdown.cancelled() => {
                            tracing::debug!(peer = %peer, "Forward proxy tunnel cancelled (shutdown)");
                        }
                    }
                    count.fetch_sub(1, Ordering::Relaxed);
                    drop(permit);
                });
            }
            _ = shutdown.cancelled() => {
                let remaining = active_count.load(Ordering::Relaxed);
                tracing::info!(active = remaining, "Forward proxy shutting down, draining connections");
                // Give active tunnels 5s to finish
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let still_active = active_count.load(Ordering::Relaxed);
                if still_active > 0 {
                    tracing::warn!(remaining = still_active, "Forward proxy: forcing shutdown with active tunnels");
                }
                break;
            }
        }
    }
}

/// Returns true if the IP address is private, loopback, or link-local.
pub fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()              // 127.0.0.0/8
            || v4.is_private()            // 10/8, 172.16/12, 192.168/16
            || v4.is_link_local()         // 169.254/16
            || v4.is_unspecified()        // 0.0.0.0
            || v4.octets()[0] == 100 && v4.octets()[1] >= 64 && v4.octets()[1] <= 127 // CGNAT 100.64/10
            || *v4 == Ipv4Addr::BROADCAST
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()              // ::1
            || v6.is_unspecified()        // ::
            || is_ipv6_private(v6)
        }
    }
}

fn is_ipv6_private(v6: &Ipv6Addr) -> bool {
    let segs = v6.segments();
    // Link-local fe80::/10
    (segs[0] & 0xffc0) == 0xfe80
    // Unique local fc00::/7
    || (segs[0] & 0xfe00) == 0xfc00
    // IPv4-mapped ::ffff:0:0/96 — check the embedded IPv4
    || (segs[0..5] == [0, 0, 0, 0, 0] && segs[5] == 0xffff && {
        let v4 = Ipv4Addr::new(
            (segs[6] >> 8) as u8, segs[6] as u8,
            (segs[7] >> 8) as u8, segs[7] as u8,
        );
        v4.is_loopback() || v4.is_private() || v4.is_link_local()
    })
}

/// Truncate domain for safe logging — prevents log flooding from excessively long domains.
fn safe_domain(domain: &str) -> String {
    if domain.len() <= 100 {
        domain.to_string()
    } else {
        format!("{}…[truncated, len={}]", &domain[..100], domain.len())
    }
}

/// Resolve a domain and validate the result is not a private IP.
/// Returns the resolved socket address string (ip:port) on success.
/// On private IP detection, records the block on the `BlockedBuffer`
/// (slice-5a observability surface, served at `GET /egress/blocked`).
async fn resolve_and_validate(
    domain: &str,
    port: u16,
    sandbox: &str,
    blocked_egress: &BlockedBuffer,
) -> anyhow::Result<String> {
    let target = format!("{domain}:{port}");
    let addrs: Vec<_> = tokio::net::lookup_host(&target).await?.collect();

    if addrs.is_empty() {
        anyhow::bail!("DNS resolution failed for {domain}");
    }

    let addr = addrs[0];
    if is_private_ip(&addr.ip()) {
        tracing::warn!(domain = %domain, resolved_ip = %addr.ip(),
            "DNS rebinding blocked: domain resolves to private IP");
        blocked_egress.record(sandbox, domain, port);
        anyhow::bail!(
            "domain {domain} resolves to private/internal IP {}",
            addr.ip()
        );
    }

    Ok(addr.to_string())
}

async fn handle_connection(
    mut stream: TcpStream,
    blocklist: &Blocklist,
    sandbox: &str,
    blocked_egress: &BlockedBuffer,
) -> anyhow::Result<()> {
    // Read the initial request. For TLS ClientHello, we may need multiple reads
    // if the handshake is fragmented across TCP segments (rare, but possible).
    let mut buf = vec![0u8; 16384];
    let mut total = 0;
    loop {
        let n = stream.read(&mut buf[total..]).await?;
        if n == 0 {
            return Ok(());
        }
        total += n;

        // For plain HTTP: stop as soon as we see the end of the first line
        if buf[..total].windows(2).any(|w| w == b"\r\n") {
            break;
        }
        // For TLS: stop once we have at least the record header + full handshake length
        if total >= 5 && buf[0] == 0x16 && buf[1] == 0x03 {
            let record_len = u16::from_be_bytes([buf[3], buf[4]]) as usize + 5;
            if total >= record_len || total >= buf.len() {
                break;
            }
        }
        if total >= buf.len() {
            break;
        }
    }
    let request = &buf[..total];

    // Parse the first line: "METHOD target HTTP/1.x\r\n"
    let first_line_end = request
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(total);
    let first_line = std::str::from_utf8(&request[..first_line_end])?;
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        send_response(&mut stream, 400, "Bad Request").await?;
        return Ok(());
    }

    let method = parts[0];
    let target = parts[1];

    if method.eq_ignore_ascii_case("CONNECT") {
        handle_connect(stream, target, request, blocklist, sandbox, blocked_egress).await
    } else {
        handle_http(
            stream,
            target,
            request,
            total,
            blocklist,
            sandbox,
            blocked_egress,
        )
        .await
    }
}

/// Handle HTTP CONNECT (HTTPS tunneling).
/// The client sends: CONNECT example.com:443 HTTP/1.1
/// We run the full egress policy (blocklist → learn → allowlist → pending), then tunnel.
async fn handle_connect(
    mut stream: TcpStream,
    target: &str,
    _request: &[u8],
    blocklist: &Blocklist,
    sandbox: &str,
    blocked_egress: &BlockedBuffer,
) -> anyhow::Result<()> {
    let (domain, port) = parse_host_port(target, 443);
    let log_dom = safe_domain(&domain);

    tracing::info!(domain = %log_dom, port = port, "CONNECT request");

    if let Err(reason) = blocklist.check_egress(&domain, sandbox).await {
        tracing::warn!(domain = %log_dom, reason = %reason, "CONNECT blocked");
        blocked_egress.record(sandbox, &domain, port);
        send_response(&mut stream, 403, "Blocked by Kars egress policy").await?;
        return Ok(());
    }

    // Resolve DNS immediately after policy check and validate against private IPs
    let resolved = match resolve_and_validate(&domain, port, sandbox, blocked_egress).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::warn!(domain = %log_dom, error = %e, "CONNECT: DNS validation failed");
            blocked_egress.record(sandbox, &domain, port);
            send_response(&mut stream, 502, "DNS validation failed").await?;
            return Ok(());
        }
    };

    // Connect to the resolved IP (not the domain string) to prevent DNS rebinding
    let upstream = match TcpStream::connect(&resolved).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(target = %resolved, error = %e, "CONNECT upstream failed");
            send_response(&mut stream, 502, "Bad Gateway").await?;
            return Ok(());
        }
    };

    // Send 200 Connection Established
    stream
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;

    // Bidirectional tunnel — we never see the plaintext (TLS between client and server)
    tunnel(stream, upstream).await;
    Ok(())
}

/// Handle plain HTTP proxy requests (or iptables-redirected HTTPS).
///
/// When iptables REDIRECT sends TCP 443 traffic here, the client is doing a TLS
/// handshake (not HTTP). We detect this via the TLS ClientHello and extract the
/// SNI hostname for blocklist checking, then tunnel to the original destination.
async fn handle_http(
    mut stream: TcpStream,
    target: &str,
    request: &[u8],
    request_len: usize,
    blocklist: &Blocklist,
    sandbox: &str,
    blocked_egress: &BlockedBuffer,
) -> anyhow::Result<()> {
    // Check if this is a TLS ClientHello (iptables-redirected HTTPS).
    // TLS records start with 0x16 (handshake) 0x03 (TLS version major).
    if request_len > 5 && request[0] == 0x16 && request[1] == 0x03 {
        return handle_tls_redirect(
            stream,
            request,
            request_len,
            blocklist,
            sandbox,
            blocked_egress,
        )
        .await;
    }

    // Plain HTTP: extract Host header
    let request_str = String::from_utf8_lossy(&request[..request_len]);
    let domain = extract_host_header(&request_str)
        .or_else(|| extract_domain_from_url(target))
        .unwrap_or_default()
        .to_lowercase();

    if domain.is_empty() {
        send_response(&mut stream, 400, "Missing Host header").await?;
        return Ok(());
    }
    let log_dom = safe_domain(&domain);

    tracing::info!(domain = %log_dom, "HTTP proxy request");

    if let Err(reason) = blocklist.check_egress(&domain, sandbox).await {
        tracing::warn!(domain = %log_dom, reason = %reason, "HTTP blocked");
        let (_h, p) = parse_host_port(&domain, 80);
        blocked_egress.record(sandbox, &domain, p);
        send_response(&mut stream, 403, "Blocked by Kars egress policy").await?;
        return Ok(());
    }

    // Resolve + validate (prevents DNS rebinding to private IPs)
    let (host, port) = parse_host_port(&domain, 80);
    let resolved = match resolve_and_validate(&host, port, sandbox, blocked_egress).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::warn!(domain = %log_dom, error = %e, "HTTP: DNS validation failed");
            blocked_egress.record(sandbox, &host, port);
            send_response(&mut stream, 502, "DNS validation failed").await?;
            return Ok(());
        }
    };

    let mut upstream = match TcpStream::connect(&resolved).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(dest = %resolved, error = %e, "HTTP upstream failed");
            send_response(&mut stream, 502, "Bad Gateway").await?;
            return Ok(());
        }
    };

    // Forward the original request to upstream
    upstream.write_all(&request[..request_len]).await?;

    // Bidirectional tunnel for the rest of the connection
    tunnel(stream, upstream).await;
    Ok(())
}

/// Handle iptables-redirected TLS connections.
/// The client thinks it's connecting directly to the server, so it sends a
/// TLS ClientHello. We parse the SNI extension to get the hostname, check
/// the blocklist, then connect to the real destination and tunnel.
async fn handle_tls_redirect(
    stream: TcpStream,
    initial_data: &[u8],
    data_len: usize,
    blocklist: &Blocklist,
    sandbox: &str,
    blocked_egress: &BlockedBuffer,
) -> anyhow::Result<()> {
    let (sni, has_ech) = extract_sni_ex(initial_data, data_len);

    if has_ech {
        // Encrypted Client Hello hides the real destination inside an encrypted
        // inner ClientHello. The outer SNI is typically a CDN domain, not the
        // true target. Allowing ECH would bypass domain-based egress policy.
        let outer_sni = sni.as_deref().unwrap_or("unknown");
        tracing::warn!(outer_sni = %outer_sni,
            "TLS redirect: Encrypted Client Hello (ECH) detected, rejecting — \
            cannot verify destination domain.");
        blocked_egress.record(sandbox, outer_sni, 443);
        return Ok(());
    }

    let domain = sni.unwrap_or_default().to_lowercase();

    if domain.is_empty() {
        // No SNI — actively reject instead of silently dropping.
        // Modern TLS clients always send SNI; absence is suspicious.
        tracing::warn!("TLS redirect: no SNI in ClientHello, rejecting connection");
        blocked_egress.record(sandbox, "<no-sni>", 443);
        return Ok(());
    }

    let log_dom = safe_domain(&domain);
    tracing::info!(domain = %log_dom, "TLS redirect (SNI)");

    if let Err(reason) = blocklist.check_egress(&domain, sandbox).await {
        tracing::warn!(domain = %log_dom, reason = %reason, "TLS blocked (SNI)");
        blocked_egress.record(sandbox, &domain, 443);
        return Ok(());
    }

    // Resolve + validate (prevents DNS rebinding to private IPs)
    let resolved = match resolve_and_validate(&domain, 443, sandbox, blocked_egress).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::warn!(domain = %log_dom, error = %e, "TLS redirect: DNS validation failed");
            blocked_egress.record(sandbox, &domain, 443);
            return Ok(());
        }
    };

    let mut upstream = match TcpStream::connect(&resolved).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(dest = %resolved, error = %e, "TLS upstream failed");
            return Ok(());
        }
    };

    // Forward the initial ClientHello to upstream
    upstream.write_all(&initial_data[..data_len]).await?;

    // Bidirectional tunnel
    tunnel(stream, upstream).await;
    Ok(())
}

/// Extract SNI (Server Name Indication) from a TLS ClientHello.
/// Also detects Encrypted Client Hello (ECH) extension and returns None
/// with a flag so the caller can reject ECH connections.
fn extract_sni_ex(data: &[u8], len: usize) -> (Option<String>, bool) {
    if len < 43 {
        return (None, false);
    }

    // TLS record: type(1) + version(2) + length(2) + handshake
    // Handshake: type(1) + length(3) + client_version(2) + random(32) = 43 bytes minimum
    let handshake_start = 5; // Skip TLS record header
    if data[handshake_start] != 0x01 {
        return (None, false); // Not a ClientHello
    }

    // Skip: handshake type(1) + length(3) + version(2) + random(32) = 38
    let mut pos = handshake_start + 38;
    if pos >= len {
        return (None, false);
    }

    // Session ID length (1 byte) + session ID
    let session_id_len = data[pos] as usize;
    pos += 1 + session_id_len;
    if pos + 2 > len {
        return (None, false);
    }

    // Cipher suites length (2 bytes) + cipher suites
    let cipher_suites_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2 + cipher_suites_len;
    if pos + 1 > len {
        return (None, false);
    }

    // Compression methods length (1 byte) + compression methods
    let comp_len = data[pos] as usize;
    pos += 1 + comp_len;
    if pos + 2 > len {
        return (None, false);
    }

    // Extensions length (2 bytes)
    let extensions_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;
    let extensions_end = pos + extensions_len;

    let mut sni: Option<String> = None;
    let mut has_ech = false;

    // Walk all extensions — extract SNI and detect ECH
    while pos + 4 <= extensions_end && pos + 4 <= len {
        let ext_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let ext_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        match ext_type {
            // SNI extension (0x0000)
            0x0000 if ext_len > 5 && pos + ext_len <= len => {
                let name_len = u16::from_be_bytes([data[pos + 3], data[pos + 4]]) as usize;
                if pos + 5 + name_len <= len {
                    sni = std::str::from_utf8(&data[pos + 5..pos + 5 + name_len])
                        .ok()
                        .map(|s| s.to_string());
                }
            }
            // Encrypted Client Hello (0xfe0d = draft ECH, 0xffce = older ESNI)
            0xfe0d | 0xffce => {
                has_ech = true;
            }
            _ => {}
        }

        pos += ext_len;
    }

    (sni, has_ech)
}

/// Parse "host:port" or bare "host", returning (host, port) with a default port.
fn parse_host_port(target: &str, default_port: u16) -> (String, u16) {
    if let Some(colon) = target.rfind(':') {
        let host = target[..colon].to_lowercase();
        let port = target[colon + 1..].parse::<u16>().unwrap_or(default_port);
        (host, port)
    } else {
        (target.to_lowercase(), default_port)
    }
}

/// Bidirectional TCP tunnel with activity-based idle timeout.
///
/// The tunnel stays open as long as data flows in either direction.
/// A 5-minute idle timer is reset on every chunk transferred.
/// This correctly supports long-lived connections like Telegram long-polling,
/// WebSocket upgrades, and SSE streams while still cleaning up zombie tunnels.
///
/// TCP keepalive is enabled on both sockets to detect silently dead connections
/// (NAT timeout, network changes, etc.) within ~90 seconds, preventing the
/// read calls from hanging indefinitely on half-open TCP connections.
async fn tunnel(mut client: TcpStream, mut upstream: TcpStream) {
    use std::time::Duration;
    use tokio::time::{Instant, sleep_until};

    const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
    const LIFETIME_CAP: Duration = Duration::from_secs(MAX_TUNNEL_LIFETIME_SECS);
    const BUF_SIZE: usize = 8192;

    // Disable Nagle's algorithm — flush every write immediately.
    // Removes up to 40ms latency on small packets (Telegram polls, WS, SSE, LLM tokens).
    client.set_nodelay(true).ok();
    upstream.set_nodelay(true).ok();

    // Enable TCP keepalive — detect silently dead connections within ~90s.
    // Without this, reads on half-open connections hang forever, blocking
    // polling loops (e.g., Telegram getUpdates) until the 300s idle timer.
    enable_tcp_keepalive(&client);
    enable_tcp_keepalive(&upstream);

    let (mut cr, mut cw) = client.split();
    let (mut ur, mut uw) = upstream.split();

    let mut c2u_buf = vec![0u8; BUF_SIZE];
    let mut u2c_buf = vec![0u8; BUF_SIZE];

    let now = Instant::now();
    let idle_timer = sleep_until(now + IDLE_TIMEOUT);
    let lifetime_timer = sleep_until(now + LIFETIME_CAP);
    tokio::pin!(idle_timer);
    tokio::pin!(lifetime_timer);

    loop {
        tokio::select! {
            result = cr.read(&mut c2u_buf) => {
                match result {
                    Ok(0) | Err(_) => break, // client closed or error
                    Ok(n) => {
                        if uw.write_all(&c2u_buf[..n]).await.is_err() {
                            break;
                        }
                        idle_timer.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                    }
                }
            }
            result = ur.read(&mut u2c_buf) => {
                match result {
                    Ok(0) | Err(_) => break, // upstream closed or error
                    Ok(n) => {
                        if cw.write_all(&u2c_buf[..n]).await.is_err() {
                            break;
                        }
                        idle_timer.as_mut().reset(Instant::now() + IDLE_TIMEOUT);
                    }
                }
            }
            _ = &mut idle_timer => {
                tracing::debug!("Forward proxy tunnel idle timeout (300s with no data)");
                break;
            }
            _ = &mut lifetime_timer => {
                tracing::info!("Forward proxy tunnel lifetime cap reached ({}s)", MAX_TUNNEL_LIFETIME_SECS);
                break;
            }
        }
    }
}

/// Enable TCP keepalive on a socket to detect silently dead connections.
///
/// Probes start after 60s idle, repeat every 10s. On most OSes this means
/// a dead connection is detected within ~90s (idle + retries × interval).
fn enable_tcp_keepalive(stream: &TcpStream) {
    use socket2::SockRef;
    use std::time::Duration;

    let sock = SockRef::from(stream);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10));
    if let Err(e) = sock.set_tcp_keepalive(&keepalive) {
        tracing::warn!(error = %e, "Failed to set TCP keepalive on tunnel socket");
    }
}

fn extract_host_header(request: &str) -> Option<&str> {
    for line in request.lines() {
        if line.to_lowercase().starts_with("host:") {
            return Some(line[5..].trim());
        }
    }
    None
}

fn extract_domain_from_url(url: &str) -> Option<&str> {
    // Absolute URL: http://example.com/path
    url.strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .and_then(|rest| rest.split('/').next())
}

async fn send_response(stream: &mut TcpStream, status: u16, body: &str) -> anyhow::Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        status = status,
        reason = match status {
            200 => "OK",
            400 => "Bad Request",
            403 => "Forbidden",
            502 => "Bad Gateway",
            _ => "Error",
        },
        len = body.len(),
        body = body,
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}
