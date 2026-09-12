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
//! | 401/407 or auth challenge | inconclusive authentication failure |
//! | 5xx | inconclusive upstream failure |
//! | other | inconclusive protocol failure |
//!
//! The scenario kind unambiguously determines [`PolicyKindRef`] for
//! denials in the v1 starter corpora (every starter case is single-
//! kind by construction; verified by the `byPolicyKind` matcher in
//! [`crate::scenarios`]). If the router later surfaces
//! `X-Azureclaw-Decision-By`, the transport will prefer that header.

use crate::outcome::ReplayError;
use anyhow::Context;
use kars_eval_corpus::{Decision, PolicyKindRef};
use reqwest::{Response, StatusCode};
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
#[path = "../../shared/mcp_content.rs"]
mod mcp_content;
#[cfg(test)]
#[path = "transport_negative_tests.rs"]
mod negative_tests;

const MAX_RESPONSE_BYTES: usize = 256 * 1024;

async fn response_body(mut response: Response) -> Result<String, ReplayError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            ReplayError::Timeout
        } else {
            ReplayError::BodyRead
        }
    })? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(ReplayError::BodyTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| ReplayError::Protocol)
}

fn decision_headers(
    headers: &reqwest::header::HeaderMap,
) -> Result<(Option<Decision>, Option<PolicyKindRef>), ReplayError> {
    if headers.contains_key("www-authenticate") || headers.contains_key("proxy-authenticate") {
        return Err(ReplayError::Authentication);
    }
    let decision = headers
        .get(DECISION_HEADER)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(parse_decision_header)
                .ok_or(ReplayError::Protocol)
        })
        .transpose()?;
    let kind = headers
        .get(DECISION_BY_HEADER)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(parse_policy_kind_header)
                .ok_or(ReplayError::Protocol)
        })
        .transpose()?;
    Ok((decision, kind))
}

fn reason_header(headers: &reqwest::header::HeaderMap) -> Result<Option<String>, ReplayError> {
    headers
        .get(DECISION_REASON_HEADER)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| ReplayError::Protocol)
        })
        .transpose()
}

/// Header names the runner reads if present. None of these are required
/// on the v1 router today; they are read opportunistically so a future
/// router slice can supply ground truth without a runner image bump.
pub const DECISION_HEADER: &str = "x-kars-decision";
pub const DECISION_BY_HEADER: &str = "x-kars-decision-by";
pub const DECISION_REASON_HEADER: &str = "x-kars-decision-reason";

/// Header echoed back by the router so its logs can be correlated with
/// runner cases. Set by the runner; the router does not need to read it.
pub const CASE_ID_HEADER: &str = "x-kars-eval-case-id";

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
            .redirect(reqwest::redirect::Policy::none())
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
/// the response carries no `x-kars-decision-by` header.
pub async fn response_to_decision(
    response: Response,
    scenario_default_kind: PolicyKindRef,
) -> Result<ActualParts, ReplayError> {
    let status = response.status();
    let headers = response.headers().clone();

    let status_decision = decision_from_status(status)?;
    let (header_decision, header_by_kind) = decision_headers(&headers)?;

    let header_reason = reason_header(&headers)?;

    let body = response_body(response).await?;
    if status.is_success() && status != StatusCode::NO_CONTENT {
        let value = serde_json::from_str::<Value>(&body).map_err(|_| ReplayError::Protocol)?;
        if !value.is_object() || value.get("error").is_some() {
            return Err(ReplayError::Protocol);
        }
    }

    let decision = header_decision.unwrap_or(status_decision);
    let by_policy_kind = header_by_kind.or(if decision == Decision::Allowed {
        None
    } else {
        Some(scenario_default_kind)
    });

    let reason = header_reason.or_else(|| reason_from_body(&body));

    Ok(ActualParts {
        decision,
        by_policy_kind,
        reason,
    })
}

/// Interpret an MCP JSON-RPC `tools/call` response.
///
/// MCP responses are always wrapped in a JSON-RPC envelope and the
/// HTTP status is almost always `200` regardless of whether the tool
/// succeeded or was denied by policy. The decision lives in the body:
///
/// - JSON-RPC `error` or `result.isError` means inconclusive protocol/tool
///   failure, not an inferred policy verdict from message substrings.
/// - A valid, correctly correlated successful result means `Allowed`.
/// - Malformed content is rejected using the router's shared typed decoder.
///
/// Non-2xx HTTP statuses fall through to [`decision_from_status`] —
/// the router's transport layer rejected the request before pipeline
/// dispatch (e.g. 413 body-size, 406 Accept, 401 OAuth).
pub async fn mcp_response_to_decision(
    response: Response,
    scenario_default_kind: PolicyKindRef,
) -> Result<ActualParts, ReplayError> {
    let status = response.status();
    let headers = response.headers().clone();

    let status_decision = decision_from_status(status)?;
    let (header_decision, header_by_kind) = decision_headers(&headers)?;
    let header_reason = reason_header(&headers)?;

    let body = response_body(response).await?;

    let (body_decision, body_reason) = if status.is_success() {
        interpret_mcp_envelope(&body).ok_or(ReplayError::Protocol)?
    } else {
        (
            status_decision,
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

    Ok(ActualParts {
        decision,
        by_policy_kind,
        reason,
    })
}

/// Parse the JSON-RPC envelope. Returns `Some((decision, reason))` if
/// the body was a valid JSON-RPC response we could interpret;
/// otherwise `None`. Protocol/tool errors are not proof of a policy denial.
fn interpret_mcp_envelope(body: &str) -> Option<(Decision, Option<String>)> {
    let v: Value = serde_json::from_str(body).ok()?;
    let obj = v.as_object()?;
    if obj.get("jsonrpc")?.as_str()? != "2.0"
        || obj.get("id")?.as_u64()? != 1
        || obj.contains_key("error")
    {
        return None;
    }
    let result = obj.get("result")?.as_object()?;
    if result
        .get("isError")
        .is_some_and(|value| value.as_bool() != Some(false))
    {
        return None;
    }
    let content = result.get("content")?.as_array()?;
    if serde_json::from_value::<Vec<mcp_content::ToolContent>>(Value::Array(content.clone()))
        .is_err()
        || result
            .get("structuredContent")
            .is_some_and(|v| !v.is_object())
        || result.get("_meta").is_some_and(|v| !v.is_object())
    {
        return None;
    }
    Some((Decision::Allowed, None))
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
/// | 502 / 504      | inconclusive     | upstream failure                     |
/// | other errors   | inconclusive     | authentication/protocol failure      |
/// | transport err  | inconclusive     | never a fabricated Blocked decision  |
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
) -> Result<ActualParts, ReplayError> {
    match tokio::time::timeout(
        timeout,
        send_connect(proxy_addr, target_host, target_port, case_id),
    )
    .await
    {
        Ok(Ok((status, reason_phrase))) => {
            let decision = decision_from_status(
                StatusCode::from_u16(status).map_err(|_| ReplayError::Protocol)?,
            )?;
            let reason = if decision == Decision::Allowed {
                None
            } else {
                Some(reason_phrase)
            };
            Ok(ActualParts {
                decision,
                by_policy_kind: if decision == Decision::Allowed {
                    None
                } else {
                    Some(PolicyKindRef::EgressAllowlist)
                },
                reason,
            })
        }
        Ok(Err(error)) => Err(error
            .downcast_ref::<ReplayError>()
            .copied()
            .unwrap_or(ReplayError::Transport)),
        Err(_) => Err(ReplayError::Timeout),
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
        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if total == buf.len() {
            break;
        }
    }

    // shut the tunnel down immediately — we never speak any payload.
    let _ = stream.shutdown().await;

    let response = String::from_utf8_lossy(&buf[..total]);
    if !response.contains("\r\n\r\n") {
        return Err(ReplayError::Protocol.into());
    }
    if response.lines().skip(1).any(|line| {
        line.split_once(':').is_some_and(|(name, _)| {
            name.eq_ignore_ascii_case("proxy-authenticate")
                || name.eq_ignore_ascii_case("www-authenticate")
        })
    }) {
        return Err(ReplayError::Authentication.into());
    }
    let first_line = response.lines().next().unwrap_or("");
    parse_http_status_line(first_line)
}

fn parse_http_status_line(line: &str) -> anyhow::Result<(u16, String)> {
    // `HTTP/1.1 200 Connection Established`
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().ok_or(ReplayError::Protocol)?;
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(ReplayError::Protocol.into());
    }
    let code = parts.next().ok_or(ReplayError::Protocol)?;
    let phrase = parts.next().unwrap_or("").trim_end().to_string();
    let code: u16 = code.parse().map_err(|_| ReplayError::Protocol)?;
    if !(100..=599).contains(&code) {
        return Err(ReplayError::Protocol.into());
    }
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

fn decision_from_status(s: StatusCode) -> Result<Decision, ReplayError> {
    if s.is_success() {
        return Ok(Decision::Allowed);
    }
    match s.as_u16() {
        402 => Ok(Decision::BudgetExceeded),
        403 => Ok(Decision::Blocked),
        429 => Ok(Decision::RateLimited),
        401 | 407 => Err(ReplayError::Authentication),
        500..=599 => Err(ReplayError::Upstream),
        _ => Err(ReplayError::Protocol),
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
        "KarsMemory" => Some(PolicyKindRef::KarsMemory),
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
    // First pass: flat string field on the envelope.
    for key in ["reason", "error", "message", "detail"] {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str())
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    // Second pass: descend into nested `error` / `detail` objects to
    // pull out `error.message` (Azure OpenAI, OpenAI, JSON-RPC) or
    // `error.reason`. This covers responses like:
    //   {"error": {"message": "filtered", "code": "content_filter"}}
    for key in ["error", "detail"] {
        if let Some(inner) = obj.get(key).and_then(|v| v.as_object()) {
            for nested in ["message", "reason", "detail"] {
                if let Some(s) = inner.get(nested).and_then(|v| v.as_str())
                    && !s.is_empty()
                {
                    return Some(s.to_string());
                }
            }
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
        response_to_decision(r, scenario_kind).await.unwrap()
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
    async fn unknown_header_decision_is_inconclusive() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/probe"))
            .respond_with(ResponseTemplate::new(403).insert_header(DECISION_HEADER, "Bogus"))
            .mount(&s)
            .await;
        let response = reqwest::get(format!("{}/probe", s.uri())).await.unwrap();
        assert_eq!(
            response_to_decision(response, PolicyKindRef::ToolPolicy)
                .await
                .unwrap_err(),
            ReplayError::Protocol
        );
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
