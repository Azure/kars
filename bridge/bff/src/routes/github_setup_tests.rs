// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SENTINEL: &str = "SECRET_SENTINEL_DO_NOT_RETURN";

#[test]
fn manifest_code_validation_rejects_paths_queries_whitespace_and_unbounded_values() {
    for code in ["abc123", "a-B_2", &"a".repeat(128)] {
        assert!(validate_manifest_code(code).is_ok());
        let url = manifest_conversion_url(code).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("api.github.com"));
        assert_eq!(url.port_or_known_default(), Some(443));
        assert!(url.username().is_empty());
        assert!(url.password().is_none());
        assert!(url.query().is_none());
        assert!(url.fragment().is_none());
        assert_eq!(
            url.path_segments().unwrap().collect::<Vec<_>>(),
            ["app-manifests", code, "conversions"]
        );
    }
    for code in [
        "",
        " ",
        " abc",
        "abc\n",
        "../app",
        "abc/def",
        "a?b=c",
        "a#b",
        "a%2fb",
        "//other.example",
        "a\\b",
        "user@other.example",
        "https://other.example",
        "é",
        &"a".repeat(129),
    ] {
        assert!(matches!(
            validate_manifest_code(code),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            manifest_conversion_url(code),
            Err(AppError::BadRequest(_))
        ));
    }
}

#[tokio::test]
async fn manifest_response_errors_do_not_echo_upstream_credentials_or_codes() {
    let pem = format!("-----BEGIN PRIVATE KEY-----\n{SENTINEL}\n-----END PRIVATE KEY-----");
    let bodies = [
        format!("not json {SENTINEL}"),
        serde_json::json!({"id":SENTINEL,"pem":pem}).to_string(),
        serde_json::json!({"id":0,"pem":pem}).to_string(),
        serde_json::json!({"id":-1,"pem":pem}).to_string(),
        serde_json::json!({"id":1.5,"pem":pem}).to_string(),
        serde_json::json!({"id":1,"pem":SENTINEL}).to_string(),
        serde_json::json!({"id":1,"pem":format!("{pem}\0")}).to_string(),
        serde_json::json!({"id":1,"pem":format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----", "a".repeat(16 * 1024))}).to_string(),
        serde_json::json!({"id":1,"pem":null,"client_secret":SENTINEL}).to_string(),
        "a".repeat(GITHUB_SETUP_RESPONSE_LIMIT + 1),
    ];
    for body in bodies {
        let error = match parse_manifest_credentials(body.as_bytes()) {
            Err(error) => error,
            Ok(_) => panic!("invalid conversion response accepted"),
        };
        assert!(!format!("{error:?}").contains(SENTINEL));
        let response = error.into_response();
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(!text.contains(SENTINEL));
        assert!(text.contains("do not retry this code"));
        assert!(text.contains("existing-App upload"));
    }
}

#[test]
fn manifest_response_extracts_only_id_and_pem_then_requires_real_rsa_verification() {
    for kind in ["PRIVATE KEY", "RSA PRIVATE KEY"] {
        let pem = format!("-----BEGIN {kind}-----\n{SENTINEL}\n-----END {kind}-----");
        let body = serde_json::json!({
            "id":123, "pem":pem, "client_secret":SENTINEL,
            "webhook_secret":SENTINEL, "slug":"must-not-trust-conversion-metadata"
        })
        .to_string();
        let credentials = parse_manifest_credentials(body.as_bytes()).unwrap();
        assert_eq!(credentials.app_id, "123");
        assert_eq!(credentials.private_key, pem);
        assert!(mint_app_jwt(&credentials.app_id, &credentials.private_key).is_err());
    }
}

#[tokio::test]
async fn github_setup_transport_rejects_redirects_errors_large_bodies_and_timeouts_without_leaks() {
    for scenario in ["ok", "redirect", "failure", "length", "stream", "timeout"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let router = axum::Router::new().fallback(move || {
            let calls = observed.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                match scenario {
                    "ok" => (StatusCode::OK, "{}").into_response(),
                    "redirect" => axum::http::Response::builder()
                        .status(StatusCode::FOUND)
                        .header("location", format!("/unexpected/{SENTINEL}"))
                        .body(Body::empty())
                        .unwrap(),
                    "failure" => (StatusCode::BAD_REQUEST, SENTINEL).into_response(),
                    "length" => "a".repeat(GITHUB_SETUP_RESPONSE_LIMIT + 1).into_response(),
                    "stream" => {
                        let stream = tokio_stream::iter(
                            (0..17).map(|_| Ok::<_, std::convert::Infallible>("a".repeat(4096))),
                        );
                        axum::http::Response::new(Body::from_stream(stream))
                    }
                    _ => {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        SENTINEL.into_response()
                    }
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let request = github_setup_client()
            .unwrap()
            .post(format!("http://{address}/{SENTINEL}"))
            .timeout(std::time::Duration::from_millis(200));
        let result = github_setup_response(request).await;
        server.abort();
        if scenario == "ok" {
            assert_eq!(result.unwrap(), b"{}");
        } else {
            let error = result.unwrap_err();
            assert!(!format!("{error:?}").contains(SENTINEL));
            let body = to_bytes(error.into_response().into_body(), 4096)
                .await
                .unwrap();
            assert!(!std::str::from_utf8(&body).unwrap().contains(SENTINEL));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{scenario}");
    }
}
