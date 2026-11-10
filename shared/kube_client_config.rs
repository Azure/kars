// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::time::Duration;

pub(crate) fn preserve_request_semantics(mut config: kube::Config) -> kube::Config {
    // kube 4 enables transport retries and removes the previous read deadline.
    // Reconciliation owns retries, especially for writes with uncertain outcomes.
    config.default_retry = false;
    config.read_timeout.get_or_insert(Duration::from_secs(295));
    config
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::ConfigMap;
    use kube::{Api, Client, api::PostParams};
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[test]
    fn preserves_previous_read_deadline_without_overriding_explicit_timeout() {
        let config = kube::Config::new("http://127.0.0.1:1".parse().unwrap());
        let mut config = preserve_request_semantics(config);
        assert!(!config.default_retry);
        assert_eq!(config.read_timeout, Some(Duration::from_secs(295)));
        config.read_timeout = Some(Duration::from_secs(15));
        assert_eq!(
            preserve_request_semantics(config).read_timeout,
            Some(Duration::from_secs(15))
        );
    }

    #[tokio::test]
    async fn transient_write_responses_are_returned_without_replay() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        for status in [429, 503, 504] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/namespaces/default/configmaps"))
                .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                    "apiVersion": "v1", "kind": "Status", "status": "Failure",
                    "message": "Transient test response", "reason": "ServiceUnavailable", "code": status
                })))
                .expect(1)
                .mount(&server)
                .await;
            let config =
                preserve_request_semantics(kube::Config::new(server.uri().parse().unwrap()));
            let api = Api::<ConfigMap>::namespaced(Client::try_from(config).unwrap(), "default");
            let object: ConfigMap = serde_json::from_value(json!({
                "apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "single-write"}
            }))
            .unwrap();
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                api.create(&PostParams::default(), &object),
            )
            .await
            .expect("transport must not delay for an implicit retry");
            assert!(matches!(result, Err(kube::Error::Api(ref error)) if error.code == status));
            server.verify().await;
        }
    }
}
