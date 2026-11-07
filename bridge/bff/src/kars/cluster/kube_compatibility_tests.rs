// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::Cluster;
use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Api, CustomResourceExt, ResourceExt,
    api::{Patch, PatchParams, WatchEvent, WatchParams},
    config::{KubeConfigOptions, Kubeconfig},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const KUBECONFIG: &str = r#"
apiVersion: v1
kind: Config
current-context: bridge
clusters:
- name: cluster
  cluster:
    server: https://cluster.example.invalid:6443
    tls-server-name: kubernetes.internal
    insecure-skip-tls-verify: false
contexts:
- name: bridge
  context: {cluster: cluster, user: bridge-user, namespace: tenant-a}
users:
- name: bridge-user
  user:
    token: compatibility-test-token
---
apiVersion: v1
kind: Config
contexts:
- name: alternate
  context: {cluster: cluster, user: exec-user, namespace: tenant-b}
users:
- name: exec-user
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1
      command: bridge-test-auth-not-executed
      args: [get-token, "--tenant=tenant-b"]
      interactiveMode: Never
      provideClusterInfo: true
"#;

#[tokio::test]
async fn kubeconfig_parser_preserves_context_auth_tls_and_multi_document_merge() {
    let parsed = Kubeconfig::from_yaml(KUBECONFIG).unwrap();
    assert_eq!(parsed.contexts.len(), 2);
    let exec = parsed
        .auth_infos
        .iter()
        .find(|user| user.name == "exec-user")
        .unwrap()
        .auth_info
        .as_ref()
        .unwrap()
        .exec
        .as_ref()
        .unwrap();
    assert_eq!(
        exec.command.as_deref(),
        Some("bridge-test-auth-not-executed")
    );
    assert_eq!(
        exec.args.as_ref().unwrap(),
        &["get-token", "--tenant=tenant-b"]
    );
    assert!(exec.provide_cluster_info);
    let alternate = kube::Config::from_custom_kubeconfig(
        parsed.clone(),
        &KubeConfigOptions {
            context: Some("alternate".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(alternate.default_namespace, "tenant-b");
    assert!(alternate.auth_info.exec.is_some());
    let config = Cluster::transport_config(
        kube::Config::from_custom_kubeconfig(parsed.clone(), &KubeConfigOptions::default())
            .await
            .unwrap(),
    );
    assert!(!config.default_retry);
    assert_eq!(
        config.read_timeout,
        Some(std::time::Duration::from_secs(295))
    );
    assert_eq!(
        config.write_timeout,
        Some(std::time::Duration::from_secs(295))
    );
    assert_eq!(
        config.connect_timeout,
        Some(std::time::Duration::from_secs(30))
    );
    assert_eq!(
        config.cluster_url,
        "https://cluster.example.invalid:6443"
            .parse::<http::Uri>()
            .unwrap()
    );
    assert_eq!(config.default_namespace, "tenant-a");
    assert_eq!(
        config.tls_server_name.as_deref(),
        Some("kubernetes.internal")
    );
    assert!(!config.accept_invalid_certs);
    assert_eq!(
        serde_json::to_value(&config.auth_info).unwrap()["token"],
        "compatibility-test-token"
    );
    assert!(
        kube::Config::from_custom_kubeconfig(
            parsed,
            &KubeConfigOptions {
                context: Some("absent".into()),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert!(Kubeconfig::from_yaml("clusters: [unterminated").is_err());
}

#[tokio::test]
async fn kubeconfig_parser_preserves_embedded_ca_and_client_certificate_fields() {
    use base64::Engine;
    // The parser must retain the configured CA, not silently use system roots.
    // Certificate validation itself is left to the TLS connector.
    let pem = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
    let ca = base64::engine::general_purpose::STANDARD.encode(pem);
    let yaml = KUBECONFIG
        .replace(
            "insecure-skip-tls-verify: false",
            &format!("insecure-skip-tls-verify: false\n    certificate-authority-data: {ca}"),
        )
        .replace(
            "token: compatibility-test-token",
            "client-certificate-data: dGVzdC1jZXJ0\n    client-key-data: dGVzdC1rZXk=",
        );
    let config = kube::Config::from_custom_kubeconfig(
        Kubeconfig::from_yaml(&yaml).unwrap(),
        &KubeConfigOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(config.root_cert, Some(vec![vec![1, 2, 3]]));
    assert!(!config.accept_invalid_certs);
    assert_eq!(
        config.auth_info.client_certificate_data.as_deref(),
        Some("dGVzdC1jZXJ0")
    );
    assert!(config.auth_info.client_key_data.is_some());
    let malformed = yaml.replace(&ca, "not-valid-base64!");
    assert!(
        kube::Config::from_custom_kubeconfig(
            Kubeconfig::from_yaml(&malformed).unwrap(),
            &KubeConfigOptions::default()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn client_preserves_authorized_fenced_patch_and_never_replays_failed_writes() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    for code in [403, 409, 422, 429, 503, 504] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = calls.clone();
        let app = Router::new().fallback(move |request: Request<Body>| {
            let captured = captured.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = axum::body::to_bytes(body, 64 * 1024).await.unwrap();
                captured.lock().unwrap().push((parts.method, parts.uri, parts.headers, serde_json::from_slice::<Value>(&body).unwrap()));
                (StatusCode::from_u16(code).unwrap(), [("retry-after", "0")], Json(json!({
                    "apiVersion":"v1", "kind":"Status", "status":"Failure", "code":code,
                    "reason":"CompatibilityFailure", "message":"upstream detail",
                    "details":{"name":"settings", "causes":[{"reason":"FieldValueInvalid", "field":"metadata.resourceVersion"}]}
                }))).into_response()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut config = kube::Config::from_custom_kubeconfig(
            Kubeconfig::from_yaml(
                &KUBECONFIG.replace("https://cluster.example.invalid:6443", &endpoint),
            )
            .unwrap(),
            &KubeConfigOptions::default(),
        )
        .await
        .unwrap();
        // Do not let the test host's proxy environment redirect loopback traffic.
        config.proxy_url = None;
        let client = kube::Client::try_from(Cluster::transport_config(config)).unwrap();
        assert_eq!(client.default_namespace(), "tenant-a");
        let api: Api<ConfigMap> = Api::namespaced(client, "tenant-a");
        let patch = json!([
            {"op":"test","path":"/metadata/uid","value":"original-uid"},
            {"op":"test","path":"/metadata/resourceVersion","value":"7"},
            {"op":"add","path":"/data/setting","value":"reviewed"}
        ]);
        let result = api
            .patch(
                "settings",
                &PatchParams::default(),
                &Patch::Json::<ConfigMap>(serde_json::from_value(patch.clone()).unwrap()),
            )
            .await;
        server.abort();
        let kube::Error::Api(status) = result.unwrap_err() else {
            panic!("expected Kubernetes status")
        };
        assert_eq!(status.code, code);
        assert!(status.is_failure());
        assert_eq!(
            status.details.as_ref().unwrap().causes[0].field,
            "metadata.resourceVersion"
        );
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "transport must not replay writes for status {code}"
        );
        let (method, uri, headers, body) = &calls[0];
        assert_eq!(*method, http::Method::PATCH);
        assert_eq!(
            uri.path(),
            "/api/v1/namespaces/tenant-a/configmaps/settings"
        );
        assert_eq!(headers["authorization"], "Bearer compatibility-test-token");
        assert_eq!(headers["content-type"], "application/json-patch+json");
        assert_eq!(*body, patch);
    }
}

#[tokio::test]
async fn watch_keeps_namespace_resource_version_bookmarks_and_expired_status() {
    use tokio_stream::StreamExt;
    let service = tower::service_fn(|request: Request<kube::client::Body>| async move {
        assert_eq!(
            request.uri().path(),
            "/api/v1/namespaces/tenant-a/configmaps"
        );
        let query = request.uri().query().unwrap();
        assert!(query.contains("watch=true"));
        assert!(query.contains("resourceVersion=7"));
        let events = [
            json!({"type":"ADDED","object":{"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"settings","namespace":"tenant-a","resourceVersion":"8"}}}),
            json!({"type":"BOOKMARK","object":{"apiVersion":"v1","kind":"ConfigMap","metadata":{"resourceVersion":"9"}}}),
            json!({"type":"ERROR","object":{"apiVersion":"v1","kind":"Status","status":"Failure","code":410,"reason":"Expired","message":"watch expired"}}),
        ];
        Ok::<_, std::convert::Infallible>(http::Response::new(Body::from(
            events
                .iter()
                .map(|event| format!("{event}\n"))
                .collect::<String>(),
        )))
    });
    let client = kube::Client::new(service, "tenant-a");
    let api: Api<ConfigMap> = Api::namespaced(client, "tenant-a");
    let mut events = Box::pin(api.watch(&WatchParams::default(), "7").await.unwrap());
    let WatchEvent::Added(object) = events.next().await.unwrap().unwrap() else {
        panic!("missing added event")
    };
    assert_eq!(object.namespace().as_deref(), Some("tenant-a"));
    assert_eq!(object.resource_version().as_deref(), Some("8"));
    let WatchEvent::Bookmark(bookmark) = events.next().await.unwrap().unwrap() else {
        panic!("missing bookmark")
    };
    assert_eq!(bookmark.metadata.resource_version, "9");
    let WatchEvent::Error(status) = events.next().await.unwrap().unwrap() else {
        panic!("missing watch error")
    };
    assert_eq!(status.code, 410);
    assert_eq!(status.reason, "Expired");
    assert!(events.next().await.is_none());
}

#[test]
fn schema_derives_preserve_namespaced_crd_identity_status_and_task_wire_shape() {
    use crate::kars::{
        approval::KarsApproval, receipt::KarsReceipt, sre_action::KarsSREAction, task::KarsTask,
        team::KarsTeam,
    };
    for (kind, crd) in [
        ("KarsApproval", KarsApproval::crd()),
        ("KarsReceipt", KarsReceipt::crd()),
        ("KarsSREAction", KarsSREAction::crd()),
        ("KarsTask", KarsTask::crd()),
        ("KarsTeam", KarsTeam::crd()),
    ] {
        assert_eq!(crd.spec.group, "kars.azure.com");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert_eq!(crd.spec.names.kind, kind);
        let version = &crd.spec.versions[0];
        assert_eq!(version.name, "v1alpha1");
        assert!(version.served && version.storage);
        assert!(version.subresources.as_ref().unwrap().status.is_some());
        let schema = serde_json::to_value(version.schema.as_ref().unwrap()).unwrap();
        assert_eq!(
            schema["openAPIV3Schema"]["properties"]["spec"]["type"],
            "object"
        );
        assert_eq!(
            schema["openAPIV3Schema"]["properties"]["status"]["type"],
            "object"
        );
    }
    let task: KarsTask = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask", "metadata":{"name":"reviewed","namespace":"tenant-a"},
        "spec":{"objective":"Review", "envelope":{"tier":2,"authorityCeiling":2}}
    })).unwrap();
    let value = serde_json::to_value(&task).unwrap();
    assert_eq!(value["spec"]["envelope"]["authorityCeiling"], 2);
    assert_eq!(value["spec"]["envelope"]["delegationDepth"], 0);
    assert!(value["spec"].get("requestedTier").is_none());
    assert!(value["spec"].get("execution").is_none());
}
