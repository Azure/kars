// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! HTTP client and response → [`ActualDecision`] mapping.
//!
//! The runner talks to a single inference router base URL — the same
//! URL the sandbox agent uses, but **without** the loopback shortcut
//! (the runner Pod is sidecar-less; it routes through the in-cluster
//! Service the controller stamps for every sandbox).
//!
//! ### Status → [`Decision`] mapping
//!
//! Until the router gains explicit `X-Azureclaw-Decision*` headers
//! (deferred to a later slice — see `slice-6-claw-eval-conformance.md
//! §6 "Router delta: none mandatory"`), the runner infers the
//! decision from the HTTP status code and reads `reason` opportunistically
//! from response headers + body:
//!
//! | Status | [`Decision`]      |
//! |--------|-------------------|
//! | 2xx    | `Allowed`         |
//! | 402    | `BudgetExceeded`  |
//! | 403    | `Blocked`         |
//! | 429    | `RateLimited`     |
//! | other  | `Blocked` (with HTTP body as reason) |
//!
//! The scenario kind unambiguously determines [`PolicyKindRef`] for
//! denials in the v1 starter corpora (every starter case is single-
//! kind by construction; verified by the `byPolicyKind` matcher in
//! [`crate::scenarios`]). If the router later surfaces
//! `X-Azureclaw-Decision-By`, the transport will prefer that header.

use anyhow::Context;
use azureclaw_eval_corpus::{Decision, PolicyKindRef};
use reqwest::{Response, StatusCode};
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Header names the runner reads if present. None of these are required
/// on the v1 router today; they are read opportunistically so a future
/// router slice can supply ground truth without a runner image bump.
pub const DECISION_HEADER: &str = "x-azureclaw-decision";
pub const DECISION_BY_HEADER: &str = "x-azureclaw-decision-by";
pub const DECISION_REASON_HEADER: &str = "x-azureclaw-decision-reason";

/// Header echoed back by the router so its logs can be correlated with
/// runner cases. Set by the runner; the router does not need to read it.
pub const CASE_ID_HEADER: &str = "x-azureclaw-eval-case-id";

#[derive(Clone)]
pub struct Transport {
    client: reqwest::Client,
    base: String,
    forward_proxy_addr: Option<String>,
    timeout: Duration,
}

impl Transport {
    pub fn new(base: impl Into<String>, timeout: Duration) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .context("build reqwest client")?;
        let base = base.into();
        let base = base.trim_end_matches('/').to_string();
        Ok(Self {
            client,
            base,
            forward_proxy_addr: None,
            timeout,
        })
    }

    /// Attach a `host:port` for the inference router's forward proxy
    /// (used by [`crate::scenarios`] for `EgressConnect` HTTP CONNECT).
    pub fn with_forward_proxy(mut self, addr: impl Into<String>) -> Self {
        self.forward_proxy_addr = Some(addr.into());
        self
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// `host:port` for `EgressConnect` HTTP CONNECT, or `None` if the
    /// caller did not configure one.
    pub fn forward_proxy_addr(&self) -> Option<&str> {
        self.forward_proxy_addr.as_deref()
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn url(&self, path: &str) -> String {
        if path.starts_with('/') {
            format!("{}{}", self.base, path)
        } else {
            format!("{}/{}", self.base, path)
        }
    }
}

/// Decompose a [`Response`] into a [`Decision`] + reason + optional
/// `byPolicyKind` override. Consumes the response body lazily — returns
/// the body text iff the runner needs it for `reasonContains`.
///
/// `scenario_default_kind` is the [`PolicyKindRef`] the scenario type
/// implies (e.g. `EgressConnect` → `EgressAllowlist`). It is used iff
/// the response carries no `x-azureclaw-decision-by` header.
pub async fn response_to_decision(
    response: Response,
    scenario_default_kind: PolicyKindRef,
) -> ActualParts {
    let status = response.status();
    let headers = response.headers().clone();

    let header_decision = headers
        .get(DECISION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_decision_header);

    let header_by_kind = headers
        .get(DECISION_BY_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_policy_kind_header);

    let header_reason = headers
        .get(DECISION_REASON_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let body = response.text().await.unwrap_or_default();

    let decision = header_decision.unwrap_or_else(|| decision_from_status(status));
    let by_policy_kind = header_by_kind.or(if decision == Decision::Allowed {
        None
    } else {
        Some(scenario_default_kind)
    });

    let reason = header_reason.or_else(|| reason_from_body(&body));

    ActualParts {
        decision,
        by_policy_kind,
        reason,
    }
}

/// Interpret an MCP JSON-RPC `tools/call` response.
///
/// MCP responses are always wrapped in a JSON-RPC envelope and the
/// HTTP status is almost always `200` regardless of whether the tool
/// succeeded or was denied by policy. The decision lives in the body:
///
/// - `error` member present (per JSON-RPC 2.0) → the *protocol* failed
///   (parse error, method not found, invalid params, etc.). The runner
///   maps these to [`Decision::Blocked`] with the error message as
///   reason.
/// - `result.isError == true` (per MCP spec) → the tool itself
///   reported failure. The runner extracts the textual content as
///   reason and maps to [`Decision::Blocked`] (with rate-limit/budget
///   heuristics on the message text).
/// - Otherwise → [`Decision::Allowed`].
///
/// Non-2xx HTTP statuses fall through to [`decision_from_status`] —
/// the router's transport layer rejected the request before pipeline
/// dispatch (e.g. 413 body-size, 406 Accept, 401 OAuth).
pub async fn mcp_response_to_decision(
    response: Response,
    scenario_default_kind: PolicyKindRef,
) -> ActualParts {
    let status = response.status();
    let headers = response.headers().clone();

    let header_decision = headers
        .get(DECISION_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_decision_header);
    let header_by_kind = headers
        .get(DECISION_BY_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_policy_kind_header);
    let header_reason = headers
        .get(DECISION_REASON_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let body = response.text().await.unwrap_or_default();

    let (body_decision, body_reason) = if status.is_success() {
        interpret_mcp_envelope(&body).unwrap_or((Decision::Allowed, None))
    } else {
        (
            decision_from_status(status),
            reason_from_body(&body).or_else(|| Some(format!("router HTTP {}", status.as_u16()))),
        )
    };

    let decision = header_decision.unwrap_or(body_decision);
    let by_policy_kind = header_by_kind.or(if decision == Decision::Allowed {
        None
    } else {
        Some(scenario_default_kind)
    });
    let reason = header_reason.or(body_reason);

    ActualParts {
        decision,
        by_policy_kind,
        reason,
    }
}

/// Parse the JSON-RPC envelope. Returns `Some((decision, reason))` if
/// the body was a valid JSON-RPC response we could interpret;
/// otherwise `None` (transport caller treats `None` as Allowed when
/// the HTTP status was 2xx).
fn interpret_mcp_envelope(body: &str) -> Option<(Decision, Option<String>)> {
    if body.is_empty() {
        return Some((Decision::Allowed, None));
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let obj = v.as_object()?;

    if let Some(err) = obj.get("error").and_then(|e| e.as_object()) {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string);
        let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
        let decision = decision_from_mcp_message(message.as_deref(), Some(code));
        return Some((decision, message));
    }

    if let Some(result) = obj.get("result").and_then(|r| r.as_object()) {
        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_error {
            let msg = extract_content_text(result);
            let decision = decision_from_mcp_message(msg.as_deref(), None);
            return Some((decision, msg));
        }
        return Some((Decision::Allowed, None));
    }

    Some((Decision::Allowed, None))
}

/// Walk `result.content[]` (MCP-spec array of typed parts) and join
/// every `text`-typed part into one string. Returns `None` if the
/// content array is empty or has no text parts.
fn extract_content_text(result: &serde_json::Map<String, Value>) -> Option<String> {
    let arr = result.get("content")?.as_array()?;
    let mut out = String::new();
    for entry in arr {
        let Some(o) = entry.as_object() else {
            continue;
        };
        if o.get("type").and_then(|t| t.as_str()) != Some("text") {
            continue;
        }
        if let Some(text) = o.get("text").and_then(|t| t.as_str()) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Heuristic mapping of an MCP error-or-content message string onto a
/// [`Decision`]. The router never speaks an explicit "decision" word
/// in error messages today; we look for the same hints the operator
/// would (case-insensitive substring scan against rate / budget /
/// policy denial vocabulary).
fn decision_from_mcp_message(message: Option<&str>, code: Option<i64>) -> Decision {
    if let Some(msg) = message {
        let lower = msg.to_ascii_lowercase();
        if lower.contains("rate limit") || lower.contains("rate-limit") || lower.contains("429") {
            return Decision::RateLimited;
        }
        if lower.contains("budget") || lower.contains("quota exceeded") || lower.contains("402") {
            return Decision::BudgetExceeded;
        }
    }
    if let Some(c) = code
        && (-32768..=-32000).contains(&c)
    {
        return Decision::Blocked;
    }
    Decision::Blocked
}

/// Send a single HTTP `CONNECT <host>:<port> HTTP/1.1` request through
/// the inference router's forward proxy (`proxy_addr` = `host:port`).
/// Returns the [`ActualParts`] derived from the proxy's status line:
///
/// | Proxy status   | Decision         | Notes                                 |
/// |----------------|------------------|---------------------------------------|
/// | 200            | `Allowed`        | tunnel established (we close it)      |
/// | 403            | `Blocked`        | blocklist hit or pending approval     |
/// | 429            | `RateLimited`    | egress rate limit                     |
/// | 502 / 504      | `Blocked`        | DNS / private-IP / upstream failure   |
/// | any 4xx/5xx    | `Blocked`        | reason text scraped from status phrase|
/// | transport err  | `Blocked`        | reason = `"transport error: ..."`     |
///
/// We never speak any bytes after the `CONNECT` request — if the
/// proxy returns 200 we immediately close the socket; the runner does
/// not need to do a real TLS handshake to know "egress allowed".
pub async fn egress_connect_via_proxy(
    proxy_addr: &str,
    target_host: &str,
    target_port: u16,
    case_id: &str,
    timeout: Duration,
) -> ActualParts {
    match tokio::time::timeout(
        timeout,
        send_connect(proxy_addr, target_host, target_port, case_id),
    )
    .await
    {
        Ok(Ok((status, reason_phrase))) => {
            let decision = match status {
                200..=299 => Decision::Allowed,
                429 => Decision::RateLimited,
                402 => Decision::BudgetExceeded,
                _ => Decision::Blocked,
            };
            let reason = if decision == Decision::Allowed {
                None
            } else {
                Some(reason_phrase)
            };
            ActualParts {
                decision,
                by_policy_kind: if decision == Decision::Allowed {
                    None
                } else {
                    Some(PolicyKindRef::EgressAllowlist)
                },
                reason,
            }
        }
        Ok(Err(e)) => ActualParts {
            decision: Decision::Blocked,
            by_policy_kind: Some(PolicyKindRef::EgressAllowlist),
            reason: Some(format!("transport error: {e}")),
        },
        Err(_) => ActualParts {
            decision: Decision::Blocked,
            by_policy_kind: Some(PolicyKindRef::EgressAllowlist),
            reason: Some(format!("CONNECT timed out after {}ms", timeout.as_millis())),
        },
    }
}

/// Open a TCP socket to `proxy_addr`, send `CONNECT host:port HTTP/1.1`
/// with the case-id header, read the status line. Returns `(status,
/// reason_phrase)`.
async fn send_connect(
    proxy_addr: &str,
    target_host: &str,
    target_port: u16,
    case_id: &str,
) -> anyhow::Result<(u16, String)> {
    let mut stream = TcpStream::connect(proxy_addr)
        .await
        .with_context(|| format!("TCP connect to forward proxy {proxy_addr}"))?;
    let req = format!(
        "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n{case_hdr}: {case_id}\r\n\r\n",
        host = target_host,
        port = target_port,
        case_hdr = CASE_ID_HEADER,
        case_id = case_id,
    );
    stream
        .write_all(req.as_bytes())
        .await
        .context("write CONNECT request")?;
    stream.flush().await.context("flush CONNECT request")?;

    // Read just the status line — we only need the first \r\n. The
    // forward proxy always responds with at least an HTTP/1.1 line,
    // followed by an empty CRLF (success) or a body (failure). Reading
    // up to 4 KiB keeps us bounded even if the proxy keeps writing.
    let mut buf = [0u8; 4096];
    let mut total = 0usize;
    loop {
        let n = stream
            .read(&mut buf[total..])
            .await
            .context("read CONNECT response")?;
        if n == 0 {
            break;
        }
        total += n;
        if buf[..total].windows(2).any(|w| w == b"\r\n") {
            break;
        }
        if total == buf.len() {
            break;
        }
    }

    // shut the tunnel down immediately — we never speak any payload.
    let _ = stream.shutdown().await;

    let response = String::from_utf8_lossy(&buf[..total]);
    let first_line = response.lines().next().unwrap_or("");
    parse_http_status_line(first_line)
}

fn parse_http_status_line(line: &str) -> anyhow::Result<(u16, String)> {
    // `HTTP/1.1 200 Connection Established`
    let mut parts = line.splitn(3, ' ');
    let _version = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty status line"))?;
    let code = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("status line missing code: {line:?}"))?;
    let phrase = parts.next().unwrap_or("").trim_end().to_string();
    let code: u16 = code
        .parse()
        .with_context(|| format!("parse HTTP status code from {line:?}"))?;
    Ok((code, phrase))
}

/// What the transport derives from a single HTTP response. The runner
/// aggregates these into `ActualDecision` (adding the burst
/// observations list when applicable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualParts {
    pub decision: Decision,
    pub by_policy_kind: Option<PolicyKindRef>,
    pub reason: Option<String>,
}

fn decision_from_status(s: StatusCode) -> Decision {
    if s.is_success() {
        return Decision::Allowed;
    }
    match s.as_u16() {
        402 => Decision::BudgetExceeded,
        403 => Decision::Blocked,
        429 => Decision::RateLimited,
        _ => Decision::Blocked,
    }
}

fn parse_decision_header(s: &str) -> Option<Decision> {
    match s {
        "Allowed" => Some(Decision::Allowed),
        "Blocked" => Some(Decision::Blocked),
        "RateLimited" => Some(Decision::RateLimited),
        "BudgetExceeded" => Some(Decision::BudgetExceeded),
        _ => None,
    }
}

fn parse_policy_kind_header(s: &str) -> Option<PolicyKindRef> {
    match s {
        "EgressAllowlist" => Some(PolicyKindRef::EgressAllowlist),
        "InferencePolicy" => Some(PolicyKindRef::InferencePolicy),
        "ToolPolicy" => Some(PolicyKindRef::ToolPolicy),
        "ClawMemory" => Some(PolicyKindRef::ClawMemory),
        "McpServer" => Some(PolicyKindRef::McpServer),
        _ => None,
    }
}

/// Best-effort: look for `"reason": "..."` or `"error": "..."` in a
/// JSON body. Returns `None` for non-JSON bodies so the verdict still
/// short-circuits if the caller did not require a reason.
fn reason_from_body(body: &str) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(body).ok()?;
    let obj = v.as_object()?;
    for key in ["reason", "error", "message", "detail"] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str())
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn fetch(server: &MockServer, scenario_kind: PolicyKindRef) -> ActualParts {
        let client = reqwest::Client::new();
        let r = client
            .get(format!("{}/probe", server.uri()))
            .send()
            .await
            .unwrap();
        response_to_decision(r, scenario_kind).await
    }

    #[tokio::test]
    async fn status_200_maps_to_allowed_no_kind() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::InferencePolicy).await;
        assert_eq!(parts.decision, Decision::Allowed);
        assert_eq!(parts.by_policy_kind, None);
    }

    #[tokio::test]
    async fn status_403_maps_to_blocked_with_scenario_kind() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403).set_body_string(r#"{"reason":"host not in allowlist"}"#),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::EgressAllowlist).await;
        assert_eq!(parts.decision, Decision::Blocked);
        assert_eq!(parts.by_policy_kind, Some(PolicyKindRef::EgressAllowlist));
        assert_eq!(parts.reason.as_deref(), Some("host not in allowlist"));
    }

    #[tokio::test]
    async fn status_429_maps_to_rate_limited() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::ToolPolicy).await;
        assert_eq!(parts.decision, Decision::RateLimited);
        assert_eq!(parts.by_policy_kind, Some(PolicyKindRef::ToolPolicy));
    }

    #[tokio::test]
    async fn status_402_maps_to_budget_exceeded() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(402))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::InferencePolicy).await;
        assert_eq!(parts.decision, Decision::BudgetExceeded);
    }

    #[tokio::test]
    async fn header_overrides_status_mapping() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header(DECISION_HEADER, "RateLimited")
                    .insert_header(DECISION_BY_HEADER, "ToolPolicy"),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::InferencePolicy).await;
        assert_eq!(parts.decision, Decision::RateLimited);
        assert_eq!(parts.by_policy_kind, Some(PolicyKindRef::ToolPolicy));
    }

    #[tokio::test]
    async fn header_reason_wins_over_body_reason() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header(DECISION_REASON_HEADER, "from-header")
                    .set_body_string(r#"{"reason":"from-body"}"#),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::EgressAllowlist).await;
        assert_eq!(parts.reason.as_deref(), Some("from-header"));
    }

    #[tokio::test]
    async fn unknown_header_decision_falls_back_to_status() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(403).insert_header(DECISION_HEADER, "Bogus"))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::ToolPolicy).await;
        assert_eq!(parts.decision, Decision::Blocked);
    }

    #[tokio::test]
    async fn empty_body_yields_no_reason() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::EgressAllowlist).await;
        assert_eq!(parts.reason, None);
    }

    #[tokio::test]
    async fn non_json_body_yields_no_reason() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403).set_body_string("Forbidden — host not in allowlist"),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::EgressAllowlist).await;
        assert_eq!(parts.reason, None);
    }

    #[tokio::test]
    async fn body_error_field_used_when_reason_absent() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"error":"content-safety: jailbreak detected"}"#),
            )
            .mount(&s)
            .await;
        let parts = fetch(&s, PolicyKindRef::InferencePolicy).await;
        assert_eq!(
            parts.reason.as_deref(),
            Some("content-safety: jailbreak detected")
        );
    }

    #[test]
    fn transport_normalises_trailing_slash() {
        let t = Transport::new("http://router.local:8443/", Duration::from_secs(1)).unwrap();
        assert_eq!(t.base(), "http://router.local:8443");
    }

    #[test]
    fn transport_url_joins_paths() {
        let t = Transport::new("http://router.local:8443", Duration::from_secs(1)).unwrap();
        assert_eq!(
            t.url("/v1/chat/completions"),
            "http://router.local:8443/v1/chat/completions"
        );
    }
}
