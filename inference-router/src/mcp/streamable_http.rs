//! Streamable HTTP transport envelope per
//! [MCP spec rev. 2025-03-26 §Streamable HTTP](https://modelcontextprotocol.io/specification/2025-03-26/basic/transports).
//!
//! This module covers transport-level concerns:
//!
//! - [`SessionId`] — newtype enforcing the spec's "visible ASCII only
//!   (`0x21..=0x7E`)" constraint. `MUST` per spec.
//! - [`validate_accept_header`] — checks the client `Accept` header
//!   listing both `application/json` and `text/event-stream`.
//! - [`MAX_FRAME_BYTES`] — default oversize-frame cap (4 MiB) enforced
//!   by future POST handlers before parsing the body.
//! - [`MCP_PROTOCOL_VERSION`] — pinned protocol version string we
//!   negotiate during `initialize`.
//!
//! Route handlers (POST `/mcp`, GET `/mcp`, DELETE `/mcp`) are NOT in
//! this PR — see `phase1/mcp-2026-streamable-http-routes`.

use std::str::FromStr;

/// MCP protocol version string we advertise during `initialize`.
/// Pinned to the 2025-03-26 spec revision (see module docs).
pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// Default oversize-frame cap (4 MiB).
///
/// Future Streamable HTTP POST handlers MUST reject any request whose
/// body exceeds this size with HTTP 413 *before* invoking the
/// JSON-RPC parser. This is a defence-in-depth budget bounding both
/// memory usage and parse time. Configurable via
/// `McpServer.spec.maxFrameBytes` once the CRD lands.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// MCP Streamable HTTP session identifier. The spec mandates:
///
/// - Globally unique and cryptographically secure.
/// - Visible ASCII only — bytes in `0x21..=0x7E`.
///
/// This newtype enforces the wire-format constraint at construction.
/// Cryptographic randomness is the *minter*'s responsibility (the
/// `initialize` route handler will use `OsRng` in a follow-up PR).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    /// Construct a session id from a candidate string. Returns
    /// `Err(InvalidSessionId::Empty)` for empty input,
    /// `Err(InvalidSessionId::InvalidChar { byte, position })` for any
    /// byte outside `0x21..=0x7E`.
    pub fn try_new(s: impl Into<String>) -> Result<Self, InvalidSessionId> {
        let s = s.into();
        if s.is_empty() {
            return Err(InvalidSessionId::Empty);
        }
        for (i, b) in s.bytes().enumerate() {
            if !(0x21..=0x7E).contains(&b) {
                return Err(InvalidSessionId::InvalidChar {
                    byte: b,
                    position: i,
                });
            }
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl FromStr for SessionId {
    type Err = InvalidSessionId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_new(s)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reasons a session-id wire value is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidSessionId {
    Empty,
    InvalidChar { byte: u8, position: usize },
}

impl std::fmt::Display for InvalidSessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "session id must not be empty"),
            Self::InvalidChar { byte, position } => write!(
                f,
                "session id contains non-visible-ASCII byte 0x{byte:02x} at position {position} (allowed range: 0x21..=0x7E)"
            ),
        }
    }
}

impl std::error::Error for InvalidSessionId {}

/// Result of inspecting a client `Accept` header for Streamable HTTP
/// compliance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptNegotiation {
    /// Client lists both `application/json` and `text/event-stream`.
    /// This is the only spec-compliant shape for a POST request body
    /// containing JSON-RPC requests.
    Both,
    /// Client lists only one of the two. Spec-compliant only for
    /// notification-only POSTs (`application/json`) or
    /// pure-listening GETs (`text/event-stream`).
    OnlyJson,
    OnlySse,
    /// Header missing or contains neither.
    Neither,
}

/// Inspect a raw `Accept` header value for spec compliance.
///
/// The spec requires POST requests to advertise both
/// `application/json` and `text/event-stream`. GET requests for
/// listening streams require `text/event-stream` only.
///
/// This function tolerates `q=` parameters and case differences per
/// RFC 7231 §5.3.2.
pub fn validate_accept_header(value: &str) -> AcceptNegotiation {
    let mut has_json = false;
    let mut has_sse = false;
    for token in value.split(',') {
        // Strip parameters (`;q=0.5`, etc.) and surrounding whitespace.
        let media_type = token.split(';').next().unwrap_or("").trim();
        if media_type.eq_ignore_ascii_case("application/json")
            || media_type.eq_ignore_ascii_case("*/*")
            || media_type.eq_ignore_ascii_case("application/*")
        {
            has_json = true;
        }
        if media_type.eq_ignore_ascii_case("text/event-stream")
            || media_type.eq_ignore_ascii_case("*/*")
            || media_type.eq_ignore_ascii_case("text/*")
        {
            has_sse = true;
        }
    }
    match (has_json, has_sse) {
        (true, true) => AcceptNegotiation::Both,
        (true, false) => AcceptNegotiation::OnlyJson,
        (false, true) => AcceptNegotiation::OnlySse,
        (false, false) => AcceptNegotiation::Neither,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_pinned() {
        assert_eq!(MCP_PROTOCOL_VERSION, "2025-03-26");
    }

    #[test]
    fn frame_cap_is_4_mib() {
        assert_eq!(MAX_FRAME_BYTES, 4 * 1024 * 1024);
    }

    #[test]
    fn session_id_accepts_visible_ascii() {
        let id = SessionId::try_new("abcXYZ123-_~/").unwrap();
        assert_eq!(id.as_str(), "abcXYZ123-_~/");
    }

    #[test]
    fn session_id_accepts_uuid_like() {
        let id = SessionId::try_new("01923aef-1234-7abc-8def-0123456789ab").unwrap();
        assert_eq!(id.as_str(), "01923aef-1234-7abc-8def-0123456789ab");
    }

    #[test]
    fn session_id_rejects_empty() {
        assert_eq!(SessionId::try_new(""), Err(InvalidSessionId::Empty));
    }

    #[test]
    fn session_id_rejects_space() {
        // 0x20 is below the allowed range.
        let err = SessionId::try_new("ab cd").unwrap_err();
        assert!(matches!(
            err,
            InvalidSessionId::InvalidChar {
                byte: 0x20,
                position: 2
            }
        ));
    }

    #[test]
    fn session_id_rejects_del() {
        // 0x7F (DEL) is above the allowed range.
        let err = SessionId::try_new("ab\x7Fcd").unwrap_err();
        assert!(matches!(
            err,
            InvalidSessionId::InvalidChar {
                byte: 0x7F,
                position: 2
            }
        ));
    }

    #[test]
    fn session_id_rejects_nul() {
        let err = SessionId::try_new("ab\0cd").unwrap_err();
        assert!(matches!(
            err,
            InvalidSessionId::InvalidChar {
                byte: 0x00,
                position: 2
            }
        ));
    }

    #[test]
    fn session_id_rejects_newline() {
        let err = SessionId::try_new("a\nb").unwrap_err();
        assert!(matches!(
            err,
            InvalidSessionId::InvalidChar {
                byte: b'\n',
                position: 1
            }
        ));
    }

    #[test]
    fn session_id_rejects_unicode() {
        // é = 0xC3 0xA9 in UTF-8 — both bytes outside visible ASCII.
        let err = SessionId::try_new("café").unwrap_err();
        assert!(matches!(err, InvalidSessionId::InvalidChar { .. }));
    }

    #[test]
    fn session_id_from_str_round_trips() {
        let id: SessionId = "valid-id-123".parse().unwrap();
        assert_eq!(id.to_string(), "valid-id-123");
    }

    #[test]
    fn accept_header_both_explicit() {
        assert_eq!(
            validate_accept_header("application/json, text/event-stream"),
            AcceptNegotiation::Both
        );
    }

    #[test]
    fn accept_header_both_with_q_params() {
        assert_eq!(
            validate_accept_header("application/json;q=0.9, text/event-stream;q=0.5"),
            AcceptNegotiation::Both
        );
    }

    #[test]
    fn accept_header_case_insensitive() {
        assert_eq!(
            validate_accept_header("Application/JSON, Text/Event-Stream"),
            AcceptNegotiation::Both
        );
    }

    #[test]
    fn accept_header_only_json() {
        assert_eq!(
            validate_accept_header("application/json"),
            AcceptNegotiation::OnlyJson
        );
    }

    #[test]
    fn accept_header_only_sse() {
        assert_eq!(
            validate_accept_header("text/event-stream"),
            AcceptNegotiation::OnlySse
        );
    }

    #[test]
    fn accept_header_wildcard_satisfies_both() {
        assert_eq!(validate_accept_header("*/*"), AcceptNegotiation::Both);
    }

    #[test]
    fn accept_header_neither() {
        assert_eq!(
            validate_accept_header("application/xml, text/html"),
            AcceptNegotiation::Neither
        );
    }

    #[test]
    fn accept_header_empty() {
        assert_eq!(validate_accept_header(""), AcceptNegotiation::Neither);
    }
}
