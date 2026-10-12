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

    async fn validate(&self) -> anyhow::Result<()> {
        let pod = self.pods.get(&self.name).await.map_err(api_error)?;
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
        self.validate().await?;
        let mut forward = ForwardGuard(
            self.pods
                .portforward(&self.name, &[PROXY_PORT])
                .await
                .map_err(api_error)?,
        );
        let mut stream = forward
            .0
            .take_stream(PROXY_PORT)
            .context("target proxy stream unavailable")?;
        // A Pod can be replaced between the first GET and the upgrade. Do not
        // send a probe until the API still reports the bound incarnation.
        self.validate().await?;
        send_connect_on_stream(&mut stream, host, port, case_id).await
    }
}

fn api_error(error: kube::Error) -> ReplayError {
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
