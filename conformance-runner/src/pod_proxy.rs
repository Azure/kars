// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Kubernetes-authenticated access to the target router's private proxy.

use anyhow::{Context, ensure};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client, Config, api::Portforwarder};

use crate::{outcome::ReplayError, transport::send_connect_on_stream};

const PROXY_PORT: u16 = 8444;

#[derive(Clone)]
pub(crate) struct PodProxy {
    pods: Api<Pod>,
    name: String,
    uid: String,
}

// Portforwarder owns a spawned task; dropping a timed-out case must cancel it.
struct ForwardGuard(Portforwarder);

impl Drop for ForwardGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl PodProxy {
    pub fn in_cluster(target: &str, uid: &str) -> anyhow::Result<Self> {
        let (namespace, name) = target
            .split_once('/')
            .filter(|(namespace, name)| {
                !namespace.is_empty() && !name.is_empty() && !name.contains('/')
            })
            .context("Pod proxy requires NAMESPACE/NAME")?;
        ensure!(!uid.is_empty(), "Pod proxy requires a nonempty UID");
        let client =
            Client::try_from(Config::incluster().context("read runner cluster identity")?)?;
        Ok(Self {
            pods: Api::namespaced(client, namespace),
            name: name.into(),
            uid: uid.into(),
        })
    }

    async fn validate(&self, stage: &'static str) -> anyhow::Result<()> {
        let pod = self
            .pods
            .get(&self.name)
            .await
            .map_err(|error| api_error(stage, error))?;
        ensure!(
            current_router(&pod, &self.uid),
            "bound target Pod is no longer a ready router"
        );
        Ok(())
    }

    pub async fn connect(
        &self,
        host: &str,
        port: u16,
        case_id: &str,
    ) -> anyhow::Result<(u16, String)> {
        self.validate("target_get_before_upgrade").await?;
        let mut forward = ForwardGuard(
            self.pods
                .portforward(&self.name, &[PROXY_PORT])
                .await
                .map_err(|error| api_error("portforward_upgrade", error))?,
        );
        let mut stream = forward
            .0
            .take_stream(PROXY_PORT)
            .context("target proxy stream unavailable")?;
        // A Pod can be replaced between the first GET and the upgrade. Do not
        // send a probe until the API still reports the bound incarnation.
        self.validate("target_get_after_upgrade").await?;
        send_connect_on_stream(&mut stream, host, port, case_id).await
    }
}

fn api_error(stage: &'static str, error: kube::Error) -> ReplayError {
    use kube::client::UpgradeConnectionError;

    // Keep diagnostics to static variants and numeric status; error text can contain credentials.
    let (category, upgrade_failure, http_status) = match &error {
        kube::Error::Api(response) => ("Api", None, Some(response.code)),
        kube::Error::UpgradeConnection(upgrade) => {
            let (cause, status) = match upgrade {
                UpgradeConnectionError::ProtocolSwitch(status) => {
                    ("ProtocolSwitch", Some(status.as_u16()))
                }
                UpgradeConnectionError::MissingUpgradeWebSocketHeader => {
                    ("MissingUpgradeWebSocketHeader", None)
                }
                UpgradeConnectionError::MissingConnectionUpgradeHeader => {
                    ("MissingConnectionUpgradeHeader", None)
                }
                UpgradeConnectionError::SecWebSocketAcceptKeyMismatch => {
                    ("SecWebSocketAcceptKeyMismatch", None)
                }
                UpgradeConnectionError::SecWebSocketProtocolMismatch => {
                    ("SecWebSocketProtocolMismatch", None)
                }
                UpgradeConnectionError::GetPendingUpgrade(_) => ("GetPendingUpgrade", None),
            };
            ("UpgradeConnection", Some(cause), status)
        }
        kube::Error::Service(_) => ("Service", None, None),
        kube::Error::HyperError(_) => ("HyperError", None, None),
        kube::Error::Auth(_) => ("Auth", None, None),
        kube::Error::RustlsTls(_) => ("RustlsTls", None, None),
        kube::Error::SerdeError(_) => ("SerdeError", None, None),
        kube::Error::BuildRequest(_) => ("BuildRequest", None, None),
        _ => ("Other", None, None),
    };
    tracing::warn!(
        stage,
        kube_error_category = category,
        upgrade_failure = ?upgrade_failure,
        http_status = ?http_status,
        "Evaluation Pod proxy API request failed"
    );
    match error {
        kube::Error::Api(response) if matches!(response.code, 401 | 403) => {
            ReplayError::Authentication
        }
        _ => ReplayError::Transport,
    }
}

fn current_router(pod: &Pod, uid: &str) -> bool {
    pod.metadata.uid.as_deref() == Some(uid)
        && pod.metadata.deletion_timestamp.is_none()
        && pod.status.as_ref().is_some_and(|status| {
            status.phase.as_deref() == Some("Running")
                && status
                    .container_statuses
                    .as_ref()
                    .is_some_and(|containers| {
                        containers.iter().any(|container| {
                            container.name == "inference-router" && container.ready
                        })
                    })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    fn capture_api_error(stage: &'static str, error: kube::Error) -> (ReplayError, String) {
        use std::sync::{Arc, Mutex};

        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || Capture(writer.clone()))
            .finish();
        let verdict = tracing::subscriber::with_default(subscriber, || api_error(stage, error));
        let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        (verdict, log)
    }

    #[test]
    fn upgrade_diagnostic_preserves_status_stage_and_transport_verdict() {
        use kube::client::UpgradeConnectionError;

        for status in [401, 403, 500] {
            let error = kube::Error::UpgradeConnection(UpgradeConnectionError::ProtocolSwitch(
                reqwest::StatusCode::from_u16(status).unwrap(),
            ));
            let (verdict, log) = capture_api_error("portforward_upgrade", error);
            assert!(matches!(verdict, ReplayError::Transport));
            assert!(log.contains("stage=\"portforward_upgrade\""), "{log}");
            assert!(
                log.contains("kube_error_category=\"UpgradeConnection\""),
                "{log}"
            );
            assert!(
                log.contains("upgrade_failure=Some(\"ProtocolSwitch\")"),
                "{log}"
            );
            assert!(
                log.contains(&format!("http_status=Some({status})")),
                "{log}"
            );
        }
        let (verdict, log) = capture_api_error(
            "portforward_upgrade",
            kube::Error::UpgradeConnection(UpgradeConnectionError::SecWebSocketProtocolMismatch),
        );
        assert!(matches!(verdict, ReplayError::Transport));
        assert!(
            log.contains("upgrade_failure=Some(\"SecWebSocketProtocolMismatch\")"),
            "{log}"
        );
        assert!(log.contains("http_status=None"), "{log}");
    }

    #[test]
    fn api_and_service_diagnostics_never_render_untrusted_error_values() {
        let sensitive =
            "Bearer token-canary https://user:password-canary@private.example/body-canary";
        for stage in ["target_get_before_upgrade", "target_get_after_upgrade"] {
            let error = kube::Error::Api(Box::new(
                serde_json::from_value(json!({
                    "code": 403, "status": "Failure", "reason": sensitive, "message": sensitive
                }))
                .unwrap(),
            ));
            let (verdict, log) = capture_api_error(stage, error);
            assert!(matches!(verdict, ReplayError::Authentication));
            assert!(log.contains(&format!("stage=\"{stage}\"")), "{log}");
            assert!(log.contains("kube_error_category=\"Api\""), "{log}");
            assert!(log.contains("http_status=Some(403)"), "{log}");
            for secret in [
                "Bearer",
                "token-canary",
                "password-canary",
                "private.example",
                "body-canary",
            ] {
                assert!(!log.contains(secret), "{log}");
            }
        }
        let (verdict, log) = capture_api_error(
            "target_get_before_upgrade",
            kube::Error::Service(std::io::Error::other(sensitive).into()),
        );
        assert!(matches!(verdict, ReplayError::Transport));
        assert!(log.contains("kube_error_category=\"Service\""), "{log}");
        assert!(log.contains("http_status=None"), "{log}");
        for secret in [
            "Bearer",
            "token-canary",
            "password-canary",
            "private.example",
            "body-canary",
        ] {
            assert!(!log.contains(secret), "{log}");
        }
    }

    #[tokio::test]
    async fn denied_upgrade_remains_a_transport_error_without_retry() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/target/pods/router"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "metadata": {"uid": "bound"},
                "status": {"phase": "Running", "containerStatuses": [{
                    "name": "inference-router", "ready": true, "restartCount": 0,
                    "image": "router", "imageID": "digest"
                }]}
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/target/pods/router/portforward"))
            .respond_with(ResponseTemplate::new(403).set_body_string("credential-canary"))
            .mount(&server)
            .await;
        let client = Client::try_from(Config::new(server.uri().parse().unwrap())).unwrap();
        let proxy = PodProxy {
            pods: Api::namespaced(client, "target"),
            name: "router".into(),
            uid: "bound".into(),
        };
        let error = proxy.connect("example.com", 443, "case").await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ReplayError>(),
            Some(ReplayError::Transport)
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn replaced_pod_never_opens_a_forward() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/namespaces/target/pods/router"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "metadata": {"uid": "replacement"}
            })))
            .mount(&server)
            .await;
        let client = Client::try_from(Config::new(server.uri().parse().unwrap())).unwrap();
        let proxy = PodProxy {
            pods: Api::namespaced(client, "target"),
            name: "router".into(),
            uid: "bound".into(),
        };
        assert!(proxy.connect("example.com", 443, "case").await.is_err());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].url.path().contains("portforward"));
    }

    #[tokio::test]
    async fn denied_cluster_identity_is_not_an_egress_denial() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "apiVersion": "v1", "kind": "Status", "status": "Failure",
                "code": 403, "reason": "Forbidden", "message": "denied"
            })))
            .mount(&server)
            .await;
        let client = Client::try_from(Config::new(server.uri().parse().unwrap())).unwrap();
        let proxy = PodProxy {
            pods: Api::namespaced(client, "target"),
            name: "router".into(),
            uid: "bound".into(),
        };
        let error = proxy.connect("example.com", 443, "case").await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ReplayError>(),
            Some(ReplayError::Authentication)
        ));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[test]
    fn only_the_live_bound_router_is_accepted() {
        let pod: Pod = serde_json::from_value(json!({
            "metadata": {"uid": "bound"},
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "inference-router", "ready": true,
                    "restartCount": 0, "image": "router", "imageID": "digest"
                }]
            }
        }))
        .unwrap();
        assert!(current_router(&pod, "bound"));
        assert!(!current_router(&pod, "replaced"));
        let mut changed = pod.clone();
        changed.status.as_mut().unwrap().phase = Some("Pending".into());
        assert!(!current_router(&changed, "bound"));
        let mut changed = pod.clone();
        changed
            .status
            .as_mut()
            .unwrap()
            .container_statuses
            .as_mut()
            .unwrap()[0]
            .ready = false;
        assert!(!current_router(&changed, "bound"));
        let mut changed = pod;
        changed.metadata.deletion_timestamp =
            Some(serde_json::from_value(json!("2026-01-01T00:00:00Z")).unwrap());
        assert!(!current_router(&changed, "bound"));
    }
}
