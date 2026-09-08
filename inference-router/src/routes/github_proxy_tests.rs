// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::github_app::tests::{TOKEN, exchange, minted};
use tower::ServiceExt;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

async fn fixture(api: &str) -> (tempfile::TempDir, Service) {
    let (directory, config) = crate::github_services::tests::fixture(api).await;
    let blocklist = Arc::new(crate::blocklist::Blocklist::disabled());
    blocklist
        .replace_allowlist(vec!["github.com".into(), "127.0.0.1".into()])
        .await;
    (
        directory,
        Service {
            config,
            client: github_app::client().unwrap(),
            blocklist,
            sandbox: Arc::new("agent".into()),
            slots: Arc::new(Semaphore::new(2)),
        },
    )
}

async fn call(service: &Service, peer: &str, uri: &str, method: Method, body: Body) -> Response {
    service_routes()
        .with_state(service.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .method(method)
                .extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap()))
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn text(response: Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), LOG_LIMIT + 1)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn github_http_peer_scope_egress_and_retired_token_have_specific_denials() {
    let server = MockServer::start().await;
    let (_directory, service) = fixture(&server.uri()).await;
    for (peer, uri, status) in [
        (
            "10.0.0.2:12",
            "/gh-api/repos/owner/repo",
            StatusCode::NOT_FOUND,
        ),
        (
            "127.0.0.1:12",
            "/gh-api/repos/other/repo",
            StatusCode::FORBIDDEN,
        ),
        (
            "127.0.0.1:12",
            "/gh-api/repos/owner/repo/../../installation",
            StatusCode::FORBIDDEN,
        ),
        ("127.0.0.1:12", "/gh-api/user", StatusCode::FORBIDDEN),
        ("127.0.0.1:12", "/v1/github-token", StatusCode::GONE),
    ] {
        let response = call(&service, peer, uri, Method::GET, Body::empty()).await;
        assert_eq!(response.status(), status, "{uri}");
    }
    service.blocklist.replace_allowlist(vec![]).await;
    let response = call(
        &service,
        "127.0.0.1:12",
        "/gh-api/repos/owner/repo",
        Method::GET,
        Body::empty(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(text(response).await.contains("egress policy"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn github_real_exchange_and_dispatch_inject_only_router_credential() {
    let server = MockServer::start().await;
    exchange(&server, "owner/repo", minted("owner/repo"), 1).await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/actions/jobs/42"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"id":42}))
                .insert_header("set-cookie", "secret-cookie")
                .insert_header("location", "https://signed.example/?secret=hidden"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (_directory, service) = fixture(&server.uri()).await;
    let app = service.config.current().await.unwrap().unwrap();
    let token = app.token("owner/repo").await.unwrap();
    let uri = "/gh-api/repos/owner/repo/actions/jobs/42".parse().unwrap();
    let mut target = github_policy::target(&uri, &Method::GET, false).unwrap();
    // Only the unit-test target is remapped to a local fake GitHub HTTP server.
    target.url = format!("{}/repos/owner/repo/actions/jobs/42", server.uri());
    let (parts, _) = Request::builder()
        .uri(uri)
        .header("authorization", "Bearer agent-supplied")
        .header("cookie", "agent-cookie")
        .header("proxy-authorization", "secret")
        .header("connection", "x-secret")
        .header("x-secret", "hidden")
        .body(())
        .unwrap()
        .into_parts();
    let response = dispatch(&service, &app, &target, parts, bytes::Bytes::new(), &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("set-cookie"));
    assert!(!response.headers().contains_key("location"));
    assert_eq!(text(response).await, "{\"id\":42}");
    let requests = server.received_requests().await.unwrap();
    let sent = requests.last().unwrap();
    assert_eq!(
        sent.headers.get("authorization").unwrap().to_str().unwrap(),
        format!("Bearer {TOKEN}")
    );
    for name in ["cookie", "proxy-authorization", "x-secret", "connection"] {
        assert!(!sent.headers.contains_key(name), "{name}");
    }
    server.verify().await;
}

#[tokio::test]
async fn github_git_dispatch_preserves_pack_bytes_and_injects_basic_auth() {
    use base64::Engine;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/owner/repo.git/git-upload-pack"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/x-git-upload-pack-result")
                .set_body_bytes(b"0008NAK\n".to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (_directory, service) = fixture(&server.uri()).await;
    let app = service.config.current().await.unwrap().unwrap();
    let target = Target {
        repo: "owner/repo".into(),
        url: format!("{}/owner/repo.git/git-upload-pack", server.uri()),
        git: true,
        logs: false,
    };
    let (parts, _) = Request::builder()
        .method(Method::POST)
        .header("git-protocol", "version=2")
        .header("authorization", "Bearer agent-token")
        .header("content-type", "text/html")
        .body(())
        .unwrap()
        .into_parts();
    let body = bytes::Bytes::from_static(b"0014command=fetch\n0000");
    let response = dispatch(&service, &app, &target, parts, body.clone(), TOKEN).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/x-git-upload-pack-result"
    );
    assert_eq!(text(response).await, "0008NAK\n");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body.as_slice(), body.as_ref());
    assert!(!requests[0].headers.contains_key("content-encoding"));
    assert_eq!(
        requests[0].headers["authorization"].to_str().unwrap(),
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{TOKEN}"))
        )
    );
    assert_eq!(requests[0].headers["git-protocol"], "version=2");
    assert_eq!(
        requests[0].headers["content-type"],
        "application/x-git-upload-pack-request"
    );
}

#[tokio::test]
async fn github_git_gzip_upload_pack_preserves_encoded_bytes_and_headers() {
    use base64::Engine;
    use flate2::{Compression, read::GzDecoder, write::GzEncoder};
    use std::io::{Read, Write};

    let server = MockServer::start().await;
    exchange(&server, "owner/repo", minted("owner/repo"), 1).await;
    Mock::given(method("POST"))
        .and(path("/owner/repo.git/git-upload-pack"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"0008NAK\n".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let (_directory, service) = fixture(&server.uri()).await;
    let app = service.config.current().await.unwrap().unwrap();
    let token = app.token("owner/repo").await.unwrap();
    let target = Target {
        repo: "owner/repo".into(),
        url: format!("{}/owner/repo.git/git-upload-pack", server.uri()),
        git: true,
        logs: false,
    };
    let packet = |line: &str| format!("{:04x}{line}", line.len() + 4);
    let mut negotiation = packet("command=fetch\n");
    negotiation.push_str("0001");
    negotiation.push_str(&packet("thin-pack\n"));
    negotiation.push_str(&packet("want 0123456789012345678901234567890123456789\n"));
    for id in 0..3000 {
        negotiation.push_str(&packet(&format!("have {id:040x}\n")));
    }
    negotiation.push_str(&packet("done\n"));
    negotiation.push_str("0000");
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(negotiation.as_bytes()).unwrap();
    let encoded = encoder.finish().unwrap();
    assert!(negotiation.len() > 1024);
    assert!(encoded.len() < negotiation.len() && encoded.len() < GIT_LIMIT);
    let (parts, _) = Request::builder()
        .method(Method::POST)
        .header("content-encoding", "GZip")
        .header("git-protocol", "version=2")
        .header("authorization", "Bearer agent-supplied")
        .header("cookie", "agent-cookie")
        .body(())
        .unwrap()
        .into_parts();
    let response = dispatch(
        &service,
        &app,
        &target,
        parts,
        encoded.clone().into(),
        &token,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(text(response).await, "0008NAK\n");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    let sent = requests.last().unwrap();
    assert_eq!(sent.body, encoded);
    assert_eq!(sent.headers["content-encoding"], "gzip");
    assert_eq!(
        sent.headers["content-type"],
        "application/x-git-upload-pack-request"
    );
    assert_eq!(sent.headers["git-protocol"], "version=2");
    assert_eq!(
        sent.headers["authorization"].to_str().unwrap(),
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"))
        )
    );
    assert!(!sent.headers.contains_key("cookie"));
    let mut decoded = String::new();
    GzDecoder::new(sent.body.as_slice())
        .read_to_string(&mut decoded)
        .unwrap();
    assert_eq!(decoded, negotiation);
    server.verify().await;
}

#[tokio::test]
async fn github_git_gzip_rejects_unsupported_or_multiple_encodings_before_auth() {
    let server = MockServer::start().await;
    let (_directory, service) = fixture(&server.uri()).await;
    for encodings in [
        vec!["br"],
        vec!["deflate"],
        vec!["identity"],
        vec![""],
        vec!["gzip, br"],
        vec!["gzip, gzip"],
        vec!["gzip;level=1"],
        vec!["gzip", "gzip"],
        vec!["gzip", "br"],
    ] {
        let mut request = Request::builder()
            .uri("/git/owner/repo.git/git-upload-pack")
            .method(Method::POST)
            .extension(ConnectInfo("127.0.0.1:12".parse::<SocketAddr>().unwrap()));
        for encoding in encodings {
            request = request.header("content-encoding", encoding);
        }
        let response = service_routes()
            .with_state(service.clone())
            .oneshot(request.body(Body::from("request-body")).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            text(response).await,
            "Only a single gzip Content-Encoding on Git POST is supported"
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert("content-encoding", "gzip".parse().unwrap());
    assert_eq!(git_request_gzip(true, &Method::POST, &headers), Ok(true));
    assert_eq!(git_request_gzip(false, &Method::POST, &headers), Err(()));
    assert_eq!(git_request_gzip(true, &Method::GET, &headers), Err(()));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn github_git_gzip_retains_compressed_wire_body_limit() {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    let server = MockServer::start().await;
    let (_directory, service) = fixture(&server.uri()).await;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::none());
    encoder.write_all(&vec![b'a'; GIT_LIMIT]).unwrap();
    let encoded = encoder.finish().unwrap();
    assert!(encoded.len() > GIT_LIMIT);
    let request = Request::builder()
        .uri("/git/owner/repo.git/git-upload-pack")
        .method(Method::POST)
        .header("content-encoding", "gzip")
        .extension(ConnectInfo("127.0.0.1:12".parse::<SocketAddr>().unwrap()))
        .body(Body::from(encoded))
        .unwrap();
    let response = service_routes()
        .with_state(service)
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        text(response).await,
        "GitHub request exceeds the service limit"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn github_401_is_redacted_invalidates_cache_and_never_replays() {
    let server = MockServer::start().await;
    exchange(&server, "owner/repo", minted("owner/repo"), 1).await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(401).set_body_string("do-not-leak-token-or-body"))
        .expect(1)
        .mount(&server)
        .await;
    let (_directory, service) = fixture(&server.uri()).await;
    let app = service.config.current().await.unwrap().unwrap();
    let token = app.token("owner/repo").await.unwrap();
    assert_eq!(crate::github_app::tests::cached_count(&app).await, 1);
    let target = Target {
        repo: "owner/repo".into(),
        url: format!("{}/repos/owner/repo/issues", server.uri()),
        git: false,
        logs: false,
    };
    let (parts, _) = Request::builder()
        .method(Method::POST)
        .body(())
        .unwrap()
        .into_parts();
    let response = dispatch(&service, &app, &target, parts, "{}".into(), &token).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        text(response).await,
        "GitHub upstream did not complete the request"
    );
    assert_eq!(crate::github_app::tests::cached_count(&app).await, 0);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    server.verify().await;
}

#[tokio::test]
async fn github_signed_log_download_is_credential_free_bounded_and_does_not_redirect() {
    let server = MockServer::start().await;
    let sink = MockServer::start().await;
    let (_directory, service) = fixture(&server.uri()).await;
    Mock::given(path("/logs"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("line one\nline two\n")
                .insert_header("authorization", TOKEN),
        )
        .expect(1)
        .mount(&server)
        .await;
    let response = download(
        &service,
        format!("{}/logs?sig=private", server.uri())
            .parse()
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("authorization"));
    assert_eq!(text(response).await, "line one\nline two\n");
    let requests = server.received_requests().await.unwrap();
    assert!(!requests[0].headers.contains_key("authorization"));
    assert!(!requests[0].headers.contains_key("cookie"));
    assert!(!requests[0].headers.contains_key("referer"));
    Mock::given(path("/redirect"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", sink.uri()))
        .expect(1)
        .mount(&server)
        .await;
    let response = download(
        &service,
        format!("{}/redirect", server.uri()).parse().unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(!response.headers().contains_key("location"));
    assert!(sink.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn github_log_tail_and_response_bounds_have_exact_outcomes() {
    let server = MockServer::start().await;
    Mock::given(path("/tail"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'a'; LOG_TAIL + 3]))
        .mount(&server)
        .await;
    let response = github_app::client()
        .unwrap()
        .get(format!("{}/tail", server.uri()))
        .send()
        .await
        .unwrap();
    let result = finish(response, LOG_LIMIT, true).await;
    assert_eq!(result.headers()["x-kars-log-truncated"], "true");
    assert_eq!(
        to_bytes(result.into_body(), LOG_TAIL).await.unwrap().len(),
        LOG_TAIL
    );
    let response = github_app::client()
        .unwrap()
        .get(format!("{}/tail", server.uri()))
        .send()
        .await
        .unwrap();
    let result = finish(response, 16, false).await;
    assert_eq!(result.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        text(result).await,
        "GitHub response exceeds the service limit"
    );
}

#[test]
fn github_path_policy_denies_encoding_traversal_queries_and_privileged_actions() {
    for (method, path) in [
        (Method::GET, "/gh-api/repos/owner/repo/%2e%2e/secrets"),
        (Method::GET, "/gh-api/repos/owner/repo/%252e%252e/secrets"),
        (
            Method::GET,
            "/gh-api/repos/owner/repo/%25252e%25252e/secrets",
        ),
        (Method::GET, "/gh-api/repos/owner/repo//actions"),
        (Method::GET, "/gh-api/repos/owner/repo/actions/jobs/0/logs"),
        (
            Method::GET,
            "/gh-api/repos/owner/repo/actions/jobs/42/logs?token=hidden",
        ),
        (Method::GET, "/gh-api/repos/owner/repo?access_token=hidden"),
        (Method::GET, "/gh-api/repos/owner/repo/issues?per_page=101"),
        (Method::GET, "/gh-api/repos/owner/repo/issues?page=1&page=2"),
        (Method::GET, "https://evil.example/gh-api/repos/owner/repo"),
        (Method::PUT, "/gh-api/repos/owner/repo/pulls/1/merge"),
        (Method::POST, "/gh-api/repos/owner/repo/pulls/1/reviews"),
        (
            Method::POST,
            "/gh-api/repos/owner/repo/actions/workflows/1/dispatches",
        ),
        (Method::POST, "/gh-api/repos/owner/repo/transfer"),
        (Method::POST, "/gh-api/repos/owner/repo/forks"),
        (Method::DELETE, "/gh-api/repos/owner/repo"),
        (
            Method::POST,
            "/git/owner/repo.git/git-receive-pack?service=git-receive-pack",
        ),
    ] {
        assert!(
            github_policy::target(&path.parse().unwrap(), &method, true).is_none(),
            "{path}"
        );
    }
    let parsed = github_policy::target(
        &"/git/OWNER/Repo.git/info/refs?service=git-upload-pack"
            .parse()
            .unwrap(),
        &Method::GET,
        false,
    )
    .unwrap();
    assert_eq!(parsed.repo, "owner/repo");
    assert_eq!(
        parsed.url,
        "https://github.com/owner/repo.git/info/refs?service=git-upload-pack"
    );
    assert!(
        github_policy::target(
            &"/git/owner/repo.git/git-receive-pack".parse().unwrap(),
            &Method::POST,
            false
        )
        .is_none()
    );
    assert!(
        github_policy::target(
            &"/git/owner/repo.git/git-receive-pack".parse().unwrap(),
            &Method::POST,
            true
        )
        .is_some()
    );
    assert!(
        github_policy::target(
            &"/gh-api/repos/owner/repo/actions/runs/42/jobs?per_page=100&page=2"
                .parse()
                .unwrap(),
            &Method::GET,
            false
        )
        .is_some()
    );
}

#[test]
fn github_log_redirects_accept_only_https_known_storage_hosts_without_userinfo() {
    assert!(
        github_policy::log_redirect(
            "https://productionresultssa0.blob.core.windows.net/log?sig=value"
        )
        .is_some()
    );
    assert!(
        github_policy::log_redirect(
            "https://pipelines.actions.githubusercontent.com/log?sig=value"
        )
        .is_some()
    );
    for url in [
        "http://productionresultssa0.blob.core.windows.net/log",
        "https://productionresultssa0.blob.core.windows.net.evil.example/log",
        "https://productionresultssa0.blob.core.windows.net@evil.example/log",
        "https://user:password@productionresultssa0.blob.core.windows.net/log",
        "https://productionresultssa0.blob.core.windows.net:444/log",
        "https://127.0.0.1/log",
        "https://169.254.169.254/log",
        "file:///etc/passwd",
        "https://api.github.com/log",
        "https://productionresultssa0.blob.core.windows.net/log#secret",
    ] {
        assert!(github_policy::log_redirect(url).is_none(), "{url}");
    }
}

#[tokio::test]
async fn github_body_and_concurrency_bounds_are_enforced_before_authentication() {
    let server = MockServer::start().await;
    let (_directory, service) = fixture(&server.uri()).await;
    let response = call(
        &service,
        "127.0.0.1:12",
        "/git/owner/repo.git/git-upload-pack",
        Method::POST,
        Body::from(vec![b'a'; GIT_LIMIT + 1]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let response = call(
        &service,
        "127.0.0.1:12",
        "/gh-api/repos/owner/repo",
        Method::GET,
        Body::from("unexpected"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let _permits = service.slots.acquire_many(2).await.unwrap();
    let response = call(
        &service,
        "127.0.0.1:12",
        "/gh-api/repos/owner/repo",
        Method::GET,
        Body::empty(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn github_redirect_rejection_and_revocation_prevent_signed_log_dispatch() {
    let server = MockServer::start().await;
    let (directory, service) = fixture(&server.uri()).await;
    let app = service.config.current().await.unwrap().unwrap();
    let target = Target {
        repo: "owner/repo".into(),
        url: "https://api.github.com/repos/owner/repo/actions/jobs/42/logs".into(),
        git: false,
        logs: true,
    };
    Mock::given(path("/logs"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "https://169.254.169.254/metadata?sig=secret"),
        )
        .mount(&server)
        .await;
    let upstream = service
        .client
        .get(format!("{}/logs", server.uri()))
        .send()
        .await
        .unwrap();
    let result = response(&service, &app, &target, upstream).await;
    assert_eq!(result.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(text(result).await, "GitHub log redirect is not permitted");
    Mock::given(path("/valid"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            "https://productionresultssa0.blob.core.windows.net/log?sig=secret",
        ))
        .mount(&server)
        .await;
    let upstream = service
        .client
        .get(format!("{}/valid", server.uri()))
        .send()
        .await
        .unwrap();
    let result = response(&service, &app, &target, upstream).await;
    assert_eq!(result.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        text(result).await,
        "GitHub log destination is not allowed by egress policy"
    );
    service
        .blocklist
        .replace_allowlist(vec!["blob.core.windows.net".into()])
        .await;
    std::fs::remove_file(directory.path().join("config.json")).unwrap();
    let upstream = service
        .client
        .get(format!("{}/valid", server.uri()))
        .send()
        .await
        .unwrap();
    let result = response(&service, &app, &target, upstream).await;
    assert_eq!(result.status(), StatusCode::CONFLICT);
    assert_eq!(
        text(result).await,
        "GitHub authority changed before log download"
    );
}
