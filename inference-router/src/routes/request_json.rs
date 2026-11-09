// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! JSON parsing for unmodified bodies extracted with Axum's default request limit.
//! Provider responses, rewritten bodies and routes with custom limits must not use this parser.

use serde::de::Error as _;
use serde_json::Value;

// Matches Axum's default Bytes/Json extraction limit, checked below through a real router.
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;

pub(super) fn parse(body: &[u8]) -> serde_json::Result<Value> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(serde_json::Error::custom(
            "request body exceeds the default byte limit",
        ));
    }
    serde_json::from_slice::<Value>(body)
}

/// Tolerant routes keep malformed ingress bytes, without retrying an unbounded
/// parse at forwarding time. Generated requests must instead retain their Value.
pub(super) fn retain(body: bytes::Bytes) -> crate::proxy::RequestBody {
    match parse(&body) {
        Ok(value) => crate::proxy::RequestBody::from_validated(body, value),
        Err(_) => crate::proxy::RequestBody::unparsed(body),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::post,
    };
    use bytes::Bytes;
    use tower::ServiceExt;

    fn padded_request(size: usize) -> Vec<u8> {
        let mut body = br#"{"model":"test","stream":true}"#.to_vec();
        body.resize(size, b' ');
        body
    }

    #[test]
    fn request_at_limit_preserves_fields() {
        let value = parse(&padded_request(MAX_REQUEST_BYTES)).unwrap();
        assert_eq!(value["model"], "test");
        assert_eq!(value["stream"], true);
    }

    #[test]
    fn request_over_limit_is_rejected_before_parsing() {
        let body = padded_request(MAX_REQUEST_BYTES + 1);
        assert!(serde_json::from_slice::<Value>(&body).is_ok());
        let error = parse(&body).unwrap_err();
        assert!(error.is_data());
        assert_eq!(
            error.to_string(),
            "request body exceeds the default byte limit"
        );
    }

    #[test]
    fn standard_json_semantics_are_preserved() {
        for body in [
            b"null".as_slice(),
            b"[]",
            b"123",
            br#"{"content":"\u00e9"}"#,
        ] {
            assert_eq!(
                parse(body).unwrap(),
                serde_json::from_slice::<Value>(body).unwrap()
            );
        }
        for body in [b"".as_slice(), b"{", b"{}{}", b"\xff"] {
            assert_eq!(
                parse(body).unwrap_err().to_string(),
                serde_json::from_slice::<Value>(body)
                    .unwrap_err()
                    .to_string()
            );
        }
    }

    #[test]
    fn standard_recursion_limit_remains_enabled() {
        let body = format!("{}0{}", "[".repeat(129), "]".repeat(129));
        let error = parse(body.as_bytes()).unwrap_err();
        assert!(error.to_string().contains("recursion limit exceeded"));
    }

    #[tokio::test]
    async fn parser_limit_matches_axum_bytes_extraction() {
        async fn handler(body: Bytes) -> StatusCode {
            assert!(parse(&body).is_ok());
            StatusCode::NO_CONTENT
        }
        let app = Router::new().route("/", post(handler));
        for (size, expected) in [
            (MAX_REQUEST_BYTES - 1, StatusCode::NO_CONTENT),
            (MAX_REQUEST_BYTES, StatusCode::NO_CONTENT),
            (MAX_REQUEST_BYTES + 1, StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/")
                        .body(Body::from(padded_request(size)))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "request bytes: {size}");
        }
    }
}
