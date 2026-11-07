// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

pub(crate) const KEY: &[u8] = include_bytes!("../../a2a-gateway/testdata/test-key.pem");
pub(crate) const TOKEN: &str = "ghs_test_installation_token_only";
const PUBLIC_KEY: &[u8] = b"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAtU6qFf8uAJQ4oBrqmKax
kBEbcZCgz+qXV3zaR7o3blqkzTs6TSHe94/P00Czy6ab1xriIl/vbgFKQMCrpFDU
cAJU8+eC6Ltj4afvJz5glcX4j9O8/SkqP/x3VEC7ABnnPgYjgOicdqrwEbgbUOPB
8qrHUMbdDRzuK4uTvFuoB65YtnGpMkcaKwfST0pF6/ABFsB0cXttPSlEmQCLu848
0THaJWAwfEk8Tcn/Y39h7U1EVlNXfoAuhciBjT+lOfGNMds79OWXaY1/d4uk2W4V
w0uuKJuNRl/I5fyN2u4ybdpExHY2//BImzk4w6tnoK+ueefUHwEABXkaqO+7HVVM
sQIDAQAB
-----END PUBLIC KEY-----";

pub(crate) fn app(api: &str, repos: &[&str]) -> Arc<GitHubApp> {
    crate::install_jsonwebtoken_crypto_provider();
    let mut app = GitHubApp::new(
        "42".into(),
        7,
        KEY,
        repos.iter().map(|repo| (*repo).into()).collect(),
        false,
        client().unwrap(),
    )
    .unwrap();
    Arc::get_mut(&mut app).unwrap().api = api.into();
    app
}

pub(crate) async fn cached_count(app: &GitHubApp) -> usize {
    app.cache.lock().await.len()
}

pub(crate) fn minted(repo: &str) -> serde_json::Value {
    serde_json::json!({
        "token": TOKEN, "expires_at": (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        "repositories":[{"full_name":repo}],
        "permissions":{"actions":"read","checks":"read","contents":"read","issues":"read",
            "metadata":"read","pull_requests":"read","statuses":"read"}
    })
}

pub(crate) async fn exchange(
    server: &MockServer,
    repo: &str,
    response: serde_json::Value,
    count: u64,
) {
    Mock::given(method("GET"))
        .and(path(format!("/repos/{repo}/installation")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id":7,"app_id":42,"suspended_at":null
        })))
        .expect(count)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/installations/7/access_tokens"))
        .respond_with(ResponseTemplate::new(201).set_body_json(response))
        .expect(count)
        .mount(server)
        .await;
}

#[tokio::test]
async fn github_exchange_verifies_provenance_and_singleflights_per_repo() {
    let server = MockServer::start().await;
    exchange(&server, "owner/repo", minted("OWNER/Repo"), 1).await;
    let app = app(&server.uri(), &["owner/repo"]);
    let (first, second) = tokio::join!(app.token("owner/repo"), app.token("owner/repo"));
    assert_eq!(first.unwrap(), TOKEN);
    assert_eq!(second.unwrap(), TOKEN);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let header = requests[0]
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    let claims = jsonwebtoken::decode::<serde_json::Value>(
        header.strip_prefix("Bearer ").unwrap(),
        &jsonwebtoken::DecodingKey::from_rsa_pem(PUBLIC_KEY).unwrap(),
        &jsonwebtoken::Validation::new(Algorithm::RS256),
    )
    .unwrap()
    .claims;
    assert_eq!(claims["iss"], "42");
    assert_eq!(
        claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
        600
    );
    assert_eq!(
        requests[1].body_json::<serde_json::Value>().unwrap()["repositories"],
        serde_json::json!(["repo"])
    );
    assert_eq!(
        requests[1].body_json::<serde_json::Value>().unwrap()["permissions"]["actions"],
        "read"
    );
    server.verify().await;
}

pub(crate) fn stateless_token() -> String {
    format!(
        "ghs_42_{}.{}.{}",
        "a".repeat(100),
        "b".repeat(200),
        "c".repeat(211)
    )
}

#[tokio::test]
async fn github_accepts_opaque_stateless_and_legacy_bearer_tokens() {
    for token in [
        TOKEN.to_string(),
        stateless_token(),
        "opaque_-.~+/token==".to_string(),
        "a".repeat(16),
        "a".repeat(4096),
    ] {
        let server = MockServer::start().await;
        let mut response = minted("owner/repo");
        response["token"] = serde_json::json!(token);
        exchange(&server, "owner/repo", response, 1).await;
        let app = app(&server.uri(), &["owner/repo"]);
        assert_eq!(app.token("owner/repo").await.unwrap(), token);
        assert_eq!(app.token("owner/repo").await.unwrap(), token);
        assert_eq!(cached_count(&app).await, 1);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.verify().await;
    }
}

#[tokio::test]
async fn github_rejects_unsafe_or_unbounded_bearer_tokens_without_caching() {
    let mut tokens = vec![
        String::new(),
        "a".repeat(15),
        "a".repeat(4097),
        "=".repeat(16),
    ];
    for invalid in [
        "\r", "\n", "\t", " ", "\0", "\u{7f}", "é", ":", "=middle", "\"",
    ] {
        tokens.push(format!("sensitive-test-token{invalid}suffix"));
    }
    for token in tokens {
        let server = MockServer::start().await;
        let mut response = minted("owner/repo");
        response["token"] = serde_json::json!(token);
        exchange(&server, "owner/repo", response, 1).await;
        let app = app(&server.uri(), &["owner/repo"]);
        let error = app.token("owner/repo").await.unwrap_err();
        assert!(matches!(
            error,
            Error::Authentication {
                stage: "token_validation",
                reason: "token_format_invalid",
                status: None
            }
        ));
        assert!(!error.to_string().contains("sensitive"));
        assert_eq!(cached_count(&app).await, 0);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.verify().await;
    }
}

#[tokio::test]
async fn github_ci_read_permissions_are_exact_in_both_token_profiles() {
    for write in [false, true] {
        let server = MockServer::start().await;
        let mut expected = minted("owner/repo");
        if write {
            for permission in ["contents", "issues", "pull_requests"] {
                expected["permissions"][permission] = serde_json::json!("write");
            }
        }
        exchange(&server, "owner/repo", expected.clone(), 1).await;
        let mut app = app(&server.uri(), &["owner/repo"]);
        Arc::get_mut(&mut app).unwrap().write = write;
        assert_eq!(app.token("owner/repo").await.unwrap(), TOKEN);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let body = requests[1].body_json::<serde_json::Value>().unwrap();
        assert_eq!(body["permissions"], expected["permissions"]);
        for permission in ["actions", "checks", "statuses", "metadata"] {
            assert_eq!(body["permissions"][permission], "read");
        }
        assert_eq!(app.cache.lock().await.len(), 1);
        server.verify().await;
    }
}

#[tokio::test]
async fn github_ci_missing_or_broadened_read_permissions_never_cache() {
    for write in [false, true] {
        for permission in ["checks", "statuses"] {
            for broadened in [false, true] {
                let server = MockServer::start().await;
                let mut response = minted("owner/repo");
                if write {
                    for writable in ["contents", "issues", "pull_requests"] {
                        response["permissions"][writable] = serde_json::json!("write");
                    }
                }
                if broadened {
                    response["permissions"][permission] = serde_json::json!("write");
                } else {
                    response["permissions"]
                        .as_object_mut()
                        .unwrap()
                        .remove(permission);
                }
                exchange(&server, "owner/repo", response, 1).await;
                let mut app = app(&server.uri(), &["owner/repo"]);
                Arc::get_mut(&mut app).unwrap().write = write;
                assert!(matches!(
                    app.token("owner/repo").await,
                    Err(Error::Authentication {
                        stage: "token_validation",
                        reason: "permissions_mismatch",
                        status: None
                    })
                ));
                assert!(app.cache.lock().await.is_empty());
                let requests = server.received_requests().await.unwrap();
                assert_eq!(requests.len(), 2);
                let body = requests[1].body_json::<serde_json::Value>().unwrap();
                assert_eq!(body["permissions"]["checks"], "read");
                assert_eq!(body["permissions"]["statuses"], "read");
                server.verify().await;
            }
        }
    }
}

#[tokio::test]
async fn github_ci_installation_permission_rejection_is_not_retried_or_downgraded() {
    for write in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/installation"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id":7,"app_id":42,"suspended_at":null
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/app/installations/7/access_tokens"))
            .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
                "message":"Requested permissions exceed installation grant"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut app = app(&server.uri(), &["owner/repo"]);
        Arc::get_mut(&mut app).unwrap().write = write;
        assert!(matches!(
            app.token("owner/repo").await,
            Err(Error::Authentication {
                stage: "token_mint",
                reason: "http_rejected",
                status: Some(422)
            })
        ));
        assert!(app.cache.lock().await.is_empty());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let body = requests[1].body_json::<serde_json::Value>().unwrap();
        assert_eq!(body["permissions"]["checks"], "read");
        assert_eq!(body["permissions"]["statuses"], "read");
        server.verify().await;
    }
}

#[tokio::test]
async fn github_cross_owner_and_installation_never_authorize_mint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id":8,"app_id":42,"suspended_at":null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let app = app(&server.uri(), &["owner/repo"]);
    assert!(matches!(app.token("other/repo").await, Err(Error::Scope)));
    assert!(matches!(
        app.token("owner/repo").await,
        Err(Error::Authentication {
            stage: "installation_validation",
            reason: "installation_mismatch",
            status: None
        })
    ));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
}

#[tokio::test]
async fn github_app_id_suspension_and_exchange_body_limit_fail_closed() {
    for details in [
        serde_json::json!({"id":7,"app_id":43,"suspended_at":null}),
        serde_json::json!({"id":7,"app_id":42,"suspended_at":"2026-09-08T00:00:00Z"}),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/installation"))
            .respond_with(ResponseTemplate::new(200).set_body_json(details))
            .expect(1)
            .mount(&server)
            .await;
        let app = app(&server.uri(), &["owner/repo"]);
        assert!(matches!(
            app.token("owner/repo").await,
            Err(Error::Authentication {
                stage: "installation_validation",
                reason: "app_mismatch" | "installation_suspended",
                status: None
            })
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/installation"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'a'; 256 * 1024 + 1]))
        .expect(1)
        .mount(&server)
        .await;
    assert!(matches!(
        app(&server.uri(), &["owner/repo"])
            .token("owner/repo")
            .await,
        Err(Error::Authentication {
            stage: "installation_lookup",
            reason: "response_too_large",
            status: Some(200)
        })
    ));
}

#[tokio::test]
async fn github_rejects_broadened_or_missing_token_provenance() {
    let mut cases = Vec::new();
    let mut wrong_owner = minted("elsewhere/repo");
    cases.push(wrong_owner.clone());
    wrong_owner["repositories"] = serde_json::json!([]);
    cases.push(wrong_owner);
    let mut permissions = minted("owner/repo");
    permissions["permissions"]["administration"] = serde_json::json!("write");
    cases.push(permissions);
    let mut expired = minted("owner/repo");
    expired["expires_at"] = serde_json::json!(Utc::now().to_rfc3339());
    cases.push(expired);
    let mut no_permissions = minted("owner/repo");
    no_permissions
        .as_object_mut()
        .unwrap()
        .remove("permissions");
    cases.push(no_permissions);
    for response in cases {
        let server = MockServer::start().await;
        exchange(&server, "owner/repo", response, 1).await;
        let app = app(&server.uri(), &["owner/repo"]);
        assert!(app.token("owner/repo").await.is_err());
        assert!(app.cache.lock().await.is_empty());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn github_exchange_errors_are_redacted_and_redirects_never_followed() {
    let server = MockServer::start().await;
    let sink = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/installation"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", sink.uri())
                .set_body_string("sensitive-upstream-token-body"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let app = app(&server.uri(), &["owner/repo"]);
    let error = app.token("owner/repo").await.unwrap_err().to_string();
    assert_eq!(
        error,
        "GitHub authentication installation_lookup: http_rejected (HTTP Some(302))"
    );
    assert!(sink.received_requests().await.unwrap().is_empty());
    assert!(app.cache.lock().await.is_empty());
}

#[tokio::test]
async fn github_authentication_diagnostics_redact_response_values() {
    let mut cases = Vec::new();
    for (field, value, stage, reason) in [
        (
            "repositories",
            serde_json::json!([{"full_name":"private/secret-repository"}]),
            "token_validation",
            "repository_mismatch",
        ),
        (
            "permissions",
            serde_json::json!({"sensitive-permission":"write"}),
            "token_validation",
            "permissions_mismatch",
        ),
        (
            "expires_at",
            serde_json::json!(Utc::now().to_rfc3339()),
            "token_validation",
            "expiry_invalid",
        ),
        (
            "token",
            serde_json::json!("sensitive-token\r\nwith-invalid-header"),
            "token_validation",
            "token_format_invalid",
        ),
        (
            "expires_at",
            serde_json::json!("sensitive-invalid-response"),
            "token_mint",
            "invalid_response",
        ),
    ] {
        let mut response = minted("owner/repo");
        response[field] = value;
        cases.push((response, stage, reason));
    }
    for (response, expected_stage, expected_reason) in cases {
        let server = MockServer::start().await;
        exchange(&server, "owner/repo", response, 1).await;
        let app = app(&server.uri(), &["owner/repo"]);
        let error = app.token("owner/repo").await.unwrap_err();
        assert!(matches!(&error, Error::Authentication { stage, reason, .. }
            if *stage == expected_stage && *reason == expected_reason));
        for formatted in [error.to_string(), format!("{error:?}")] {
            for private in [
                "sensitive",
                "secret-repository",
                TOKEN,
                "owner/repo",
                &server.uri(),
            ] {
                assert!(
                    !formatted.contains(private),
                    "diagnostic leaked response or request data"
                );
            }
        }
        assert!(app.cache.lock().await.is_empty());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        server.verify().await;
    }
}

#[tokio::test]
async fn github_authentication_lookup_diagnostics_distinguish_http_and_invalid_json() {
    for (status, reason) in [
        (401, "http_rejected"),
        (403, "http_rejected"),
        (200, "invalid_response"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/installation"))
            .respond_with(ResponseTemplate::new(status).set_body_string("sensitive-upstream-body"))
            .expect(1)
            .mount(&server)
            .await;
        let app = app(&server.uri(), &["owner/repo"]);
        let error = app.token("owner/repo").await.unwrap_err();
        assert!(matches!(error, Error::Authentication {
            stage: "installation_lookup", reason: actual_reason, status: Some(actual_status)
        } if actual_reason == reason && actual_status == status));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert!(app.cache.lock().await.is_empty());
        server.verify().await;
    }
}

#[tokio::test]
async fn github_cache_is_per_credential_and_rejected_tokens_are_not_replayed() {
    let server = MockServer::start().await;
    exchange(&server, "owner/repo", minted("owner/repo"), 3).await;
    let first = app(&server.uri(), &["owner/repo"]);
    let second = app(&server.uri(), &["owner/repo"]);
    assert_eq!(first.token("owner/repo").await.unwrap(), TOKEN);
    first.invalidate("owner/repo", "different-token").await;
    assert_eq!(first.token("owner/repo").await.unwrap(), TOKEN);
    assert_eq!(second.token("owner/repo").await.unwrap(), TOKEN);
    first.invalidate("owner/repo", TOKEN).await;
    assert!(first.cache.lock().await.is_empty());
    assert_eq!(first.token("owner/repo").await.unwrap(), TOKEN);
    server.verify().await;
}

#[tokio::test]
async fn github_cache_refreshes_expiry_without_cross_repository_token_reuse() {
    let server = MockServer::start().await;
    let app = app(&server.uri(), &["owner/repo", "owner/second"]);
    for (repo, count) in [("repo", 2_u64), ("second", 1_u64)] {
        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/{repo}/installation")))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id":7,"app_id":42,"suspended_at":null
            })))
            .expect(count)
            .mount(&server)
            .await;
        let mut response = minted(&format!("owner/{repo}"));
        response["token"] = serde_json::json!(format!("{TOKEN}_{repo}"));
        Mock::given(method("POST"))
            .and(path("/app/installations/7/access_tokens"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "repositories":[repo]
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(response))
            .expect(count)
            .mount(&server)
            .await;
    }
    assert_eq!(
        app.token("owner/repo").await.unwrap(),
        format!("{TOKEN}_repo")
    );
    assert_eq!(
        app.token("owner/second").await.unwrap(),
        format!("{TOKEN}_second")
    );
    app.cache
        .lock()
        .await
        .get_mut("owner/repo")
        .unwrap()
        .expires = Utc::now().timestamp() + 30;
    assert_eq!(
        app.token("owner/repo").await.unwrap(),
        format!("{TOKEN}_repo")
    );
    assert_eq!(
        app.token("owner/second").await.unwrap(),
        format!("{TOKEN}_second")
    );
    assert_eq!(app.cache.lock().await.len(), 2);
    server.verify().await;
}

#[test]
fn github_repository_scope_is_exact_and_fail_closed() {
    assert_eq!(repository("OWNER/Repo"), Some("owner/repo".into()));
    for value in [
        "owner",
        "owner/repo/extra",
        "../repo",
        "owner/..",
        "owner/%2e",
        "https://github.com/owner/repo",
        " owner/repo",
        "owner/repo?x",
        "owner/repo#x",
        "owner\\repo",
    ] {
        assert!(repository(value).is_none(), "{value}");
    }
}
