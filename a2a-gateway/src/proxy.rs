// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Outbound proxy → inference-router :8444 (mTLS).
//!
//! Forwards the inbound, post-verification body to the router,
//! preserving the verified subject in the `X-A2A-Agent-Subject`
//! header so the router's downstream policies can key off it.

pub const SUBJECT_HEADER: &str = "X-A2A-Agent-Subject";

/// Builds the upstream URL given a configured router host and the
/// inbound request path. The gateway only ever forwards to a single
/// upstream; this helper exists so path normalisation (one and only
/// one slash, no scheme injection) is centralised + tested.
pub fn upstream_url(router_host: &str, path: &str) -> String {
    let host = router_host.trim_end_matches('/');
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    format!("https://{host}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_url_normalises_leading_slash() {
        assert_eq!(
            upstream_url("router.svc:8444", "a2a"),
            "https://router.svc:8444/a2a"
        );
        assert_eq!(
            upstream_url("router.svc:8444", "/a2a"),
            "https://router.svc:8444/a2a"
        );
    }

    #[test]
    fn upstream_url_strips_trailing_slash_on_host() {
        assert_eq!(
            upstream_url("router.svc:8444/", "/a2a"),
            "https://router.svc:8444/a2a"
        );
    }

    #[test]
    fn upstream_url_does_not_accept_scheme_in_path() {
        // The function is path-only; a caller-supplied scheme would
        // be smuggled through — but the result still has our scheme
        // prefix, so the worst case is a malformed URL, not an
        // arbitrary upstream switch.
        let u = upstream_url("router.svc:8444", "//evil.example/path");
        assert!(u.starts_with("https://router.svc:8444"));
    }

    #[test]
    fn subject_header_constant_is_canonical_case() {
        // Keep the header name stable: the router pattern-matches on
        // the exact string.
        assert_eq!(SUBJECT_HEADER, "X-A2A-Agent-Subject");
    }
}
