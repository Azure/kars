//! Transparent HTTP forward proxy for egress enforcement.
//!
//! Listens on a separate port (default 8444) and handles:
//!   - HTTP CONNECT (HTTPS tunneling): extract domain, check blocklist, then tunnel
//!   - Plain HTTP proxy: extract Host header, check blocklist, then proxy
//!
//! iptables REDIRECT sends all outbound TCP 80/443 from UID 1000 to this port,
//! making the proxy completely transparent to applications in the sandbox.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::blocklist::Blocklist;

/// Start the transparent forward proxy on the given address.
pub async fn start(addr: &str, blocklist: Blocklist) {
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, addr = %addr, "Forward proxy failed to bind");
            return;
        }
    };

    tracing::info!(addr = %addr, "Forward proxy listening (transparent egress)");
    let blocklist = Arc::new(blocklist);

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "Forward proxy accept error");
                continue;
            }
        };

        let bl = blocklist.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &bl).await {
                tracing::debug!(peer = %peer, error = %e, "Forward proxy connection error");
            }
        });
    }
}

async fn handle_connection(mut stream: TcpStream, blocklist: &Blocklist) -> anyhow::Result<()> {
    // Read the initial request line to determine HTTP method.
    // We peek at the first chunk to decide CONNECT vs plain HTTP.
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let request = &buf[..n];

    // Parse the first line: "METHOD target HTTP/1.x\r\n"
    let first_line_end = request
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(n);
    let first_line = std::str::from_utf8(&request[..first_line_end])?;
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        send_response(&mut stream, 400, "Bad Request").await?;
        return Ok(());
    }

    let method = parts[0];
    let target = parts[1];

    if method.eq_ignore_ascii_case("CONNECT") {
        // HTTPS tunneling: target is "host:port"
        handle_connect(stream, target, request, blocklist).await
    } else {
        // Plain HTTP proxy or redirected HTTPS (iptables REDIRECT changes destination
        // but the client thinks it's talking to the real server, so it sends a plain
        // HTTP request with a relative path, not an absolute URL).
        handle_http(stream, target, request, n, blocklist).await
    }
}

/// Handle HTTP CONNECT (HTTPS tunneling).
/// The client sends: CONNECT example.com:443 HTTP/1.1
/// We check the domain against the blocklist, then tunnel bytes bidirectionally.
async fn handle_connect(
    mut stream: TcpStream,
    target: &str,
    _request: &[u8],
    blocklist: &Blocklist,
) -> anyhow::Result<()> {
    // Extract domain (strip port)
    let domain = target
        .split(':')
        .next()
        .unwrap_or(target)
        .to_lowercase();

    tracing::debug!(domain = %domain, "CONNECT request");

    // Record domain in learn mode (before blocklist check — record all attempts)
    blocklist.record_learned(&domain).await;

    // Blocklist check
    if blocklist.is_blocked(&domain).await.is_blocked() {
        tracing::warn!(domain = %domain, "CONNECT blocked by egress policy");
        send_response(&mut stream, 403, "Blocked by AzureClaw egress policy").await?;
        return Ok(());
    }

    // Connect to the actual destination
    let upstream = match TcpStream::connect(target).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(target = %target, error = %e, "CONNECT upstream failed");
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
) -> anyhow::Result<()> {
    // Check if this is a TLS ClientHello (iptables-redirected HTTPS).
    // TLS records start with 0x16 (handshake) 0x03 (TLS version major).
    if request_len > 5 && request[0] == 0x16 && request[1] == 0x03 {
        return handle_tls_redirect(stream, request, request_len, blocklist).await;
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

    tracing::debug!(domain = %domain, "HTTP proxy request");

    blocklist.record_learned(&domain).await;

    if blocklist.is_blocked(&domain).await.is_blocked() {
        tracing::warn!(domain = %domain, "HTTP blocked by egress policy");
        send_response(&mut stream, 403, "Blocked by AzureClaw egress policy").await?;
        return Ok(());
    }

    // Connect to actual destination (port 80 if not specified)
    let dest = if domain.contains(':') {
        domain.clone()
    } else {
        format!("{domain}:80")
    };

    let mut upstream = match TcpStream::connect(&dest).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(dest = %dest, error = %e, "HTTP upstream failed");
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
) -> anyhow::Result<()> {
    let domain = extract_sni(initial_data, data_len)
        .unwrap_or_default()
        .to_lowercase();

    if domain.is_empty() {
        tracing::debug!("TLS redirect: no SNI found, allowing");
        // No SNI — can't determine domain. Allow but log.
        // In practice, modern TLS clients always send SNI.
    } else {
        tracing::debug!(domain = %domain, "TLS redirect (SNI)");

        blocklist.record_learned(&domain).await;

        if blocklist.is_blocked(&domain).await.is_blocked() {
            tracing::warn!(domain = %domain, "TLS blocked by egress policy (SNI)");
            // For TLS, we can't send an HTTP response — just close the connection.
            return Ok(());
        }
    }

    // Recover the original destination. iptables REDIRECT changes the destination
    // to localhost:8444, but SO_ORIGINAL_DST can recover it on Linux.
    // Fallback: use SNI domain + port 443.
    let dest = if !domain.is_empty() {
        format!("{domain}:443")
    } else {
        // Without SNI and without SO_ORIGINAL_DST, we can't route.
        tracing::warn!("TLS redirect: no SNI and no original dest, dropping");
        return Ok(());
    };

    let mut upstream = match TcpStream::connect(&dest).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(dest = %dest, error = %e, "TLS upstream failed");
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
fn extract_sni(data: &[u8], len: usize) -> Option<String> {
    if len < 43 {
        return None; // Too short for a ClientHello
    }

    // TLS record: type(1) + version(2) + length(2) + handshake
    // Handshake: type(1) + length(3) + client_version(2) + random(32) = 43 bytes minimum
    let handshake_start = 5; // Skip TLS record header
    if data[handshake_start] != 0x01 {
        return None; // Not a ClientHello
    }

    // Skip: handshake type(1) + length(3) + version(2) + random(32) = 38
    let mut pos = handshake_start + 38;
    if pos >= len {
        return None;
    }

    // Session ID length (1 byte) + session ID
    let session_id_len = data[pos] as usize;
    pos += 1 + session_id_len;
    if pos + 2 > len {
        return None;
    }

    // Cipher suites length (2 bytes) + cipher suites
    let cipher_suites_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2 + cipher_suites_len;
    if pos + 1 > len {
        return None;
    }

    // Compression methods length (1 byte) + compression methods
    let comp_len = data[pos] as usize;
    pos += 1 + comp_len;
    if pos + 2 > len {
        return None;
    }

    // Extensions length (2 bytes)
    let extensions_len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
    pos += 2;
    let extensions_end = pos + extensions_len;

    // Walk extensions looking for SNI (type 0x0000)
    while pos + 4 <= extensions_end && pos + 4 <= len {
        let ext_type = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let ext_len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;

        if ext_type == 0x0000 && ext_len > 5 && pos + ext_len <= len {
            // SNI extension: list_length(2) + type(1) + name_length(2) + name
            let name_len = u16::from_be_bytes([data[pos + 3], data[pos + 4]]) as usize;
            if pos + 5 + name_len <= len {
                return std::str::from_utf8(&data[pos + 5..pos + 5 + name_len])
                    .ok()
                    .map(|s| s.to_string());
            }
        }

        pos += ext_len;
    }

    None
}

/// Bidirectional TCP tunnel.
async fn tunnel(mut client: TcpStream, mut upstream: TcpStream) {
    let (mut cr, mut cw) = client.split();
    let (mut ur, mut uw) = upstream.split();

    let c2u = tokio::io::copy(&mut cr, &mut uw);
    let u2c = tokio::io::copy(&mut ur, &mut cw);

    // When either direction closes, we're done
    tokio::select! {
        _ = c2u => {},
        _ = u2c => {},
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
