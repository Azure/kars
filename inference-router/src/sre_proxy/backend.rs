// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Source {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Config {
    pub schema: String,
    pub kube_url: String,
    pub registration_uid: String,
    pub privacy_epoch: String,
    pub source: Source,
    pub runtime_namespace: String,
    pub namespace_uid: String,
    pub service_account_uid: String,
    pub secret_uid: String,
}

struct Token {
    value: String,
    expiry: DateTime<Utc>,
}

fn transport_failure(stage: &'static str, error: &reqwest::Error) {
    tracing::warn!(
        stage,
        timed_out = error.is_timeout(),
        connect_error = error.is_connect(),
        "SRE authority transport failure"
    );
}

fn denied_response(stage: &'static str, status: reqwest::StatusCode) {
    tracing::warn!(
        stage,
        http_status = status.as_u16(),
        "SRE authority request denied"
    );
}

pub(super) struct Backend {
    pub config: Config,
    client: reqwest::Client,
    token: Mutex<Token>,
    directory: PathBuf,
}

fn token_files(directory: &Path) -> Result<Token, String> {
    let value = std::fs::read_to_string(directory.join("kube-token"))
        .map_err(|_| "Private SRE token file unavailable")?;
    let expiry = std::fs::read_to_string(directory.join("kube-expires-at"))
        .map_err(|_| "Private SRE expiry file unavailable")?;
    let expiry = DateTime::parse_from_rfc3339(expiry.trim())
        .map_err(|_| "Private SRE expiry is invalid")?
        .with_timezone(&Utc);
    if value.trim().is_empty() {
        return Err("Private SRE token is empty".into());
    }
    Ok(Token {
        value: value.trim().into(),
        expiry,
    })
}

impl Backend {
    #[cfg(test)]
    pub(super) fn for_test(config: Config, directory: PathBuf, expiry: DateTime<Utc>) -> Arc<Self> {
        Arc::new(Self {
            config,
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            token: Mutex::new(Token {
                value: "private-kubernetes-token".into(),
                expiry,
            }),
            directory,
        })
    }

    pub(super) fn load(directory: &Path) -> Result<Arc<Self>, String> {
        let config: Config = serde_json::from_slice(
            &std::fs::read(directory.join("config.json"))
                .map_err(|_| "Private SRE configuration unavailable")?,
        )
        .map_err(|_| "Private SRE configuration invalid")?;
        let url =
            reqwest::Url::parse(&config.kube_url).map_err(|_| "Private Kubernetes URL invalid")?;
        if config.schema != "kars.azure.com/sre-api/v1"
            || config.runtime_namespace != "kars-sre"
            || config.source.name != "sre"
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || [
                config.registration_uid.as_str(),
                config.privacy_epoch.as_str(),
                config.source.namespace.as_str(),
                config.source.uid.as_str(),
                config.namespace_uid.as_str(),
                config.service_account_uid.as_str(),
                config.secret_uid.as_str(),
            ]
            .iter()
            .any(|value| value.is_empty())
        {
            return Err("Private SRE identity is incomplete or invalid".into());
        }
        let ca = std::fs::read(directory.join("kube-ca.crt"))
            .map_err(|_| "Kubernetes CA unavailable")?;
        let certificate =
            reqwest::Certificate::from_pem(&ca).map_err(|_| "Kubernetes CA invalid")?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(certificate)
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|_| "SRE Kubernetes client could not initialize")?;
        Ok(Arc::new(Self {
            config,
            client,
            token: Mutex::new(token_files(directory)?),
            directory: directory.into(),
        }))
    }

    pub(super) async fn bearer(&self) -> Result<String, String> {
        let mut token = self.token.lock().await;
        if token.expiry > Utc::now() + chrono::Duration::minutes(5) {
            return Ok(token.value.clone());
        }
        if let Ok(updated) = token_files(&self.directory)
            && updated.expiry > token.expiry
        {
            *token = updated;
        }
        if token.expiry <= Utc::now() {
            return Err("Private SRE Kubernetes credential expired; no ambient fallback".into());
        }
        let path = format!(
            "/api/v1/namespaces/{}/serviceaccounts/sre-api-router/token",
            self.config.runtime_namespace
        );
        let response=self.client.post(format!("{}{}",self.config.kube_url.trim_end_matches('/'),path))
            .bearer_auth(&token.value).json(&json!({
                "apiVersion":"authentication.k8s.io/v1","kind":"TokenRequest",
                "spec":{"audiences":[],"expirationSeconds":3600,"boundObjectRef":{
                    "apiVersion":"v1","kind":"Secret","name":"sre-api-router-identity","uid":self.config.secret_uid}},
            })).send().await.map_err(|_|"SRE token renewal transport failure")?;
        if !response.status().is_success() {
            return Err("SRE token renewal was denied; no ambient fallback".into());
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| "SRE token renewal response invalid")?;
        let new_token = value["status"]["token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("SRE token renewal omitted token")?;
        let expiry = value["status"]["expirationTimestamp"]
            .as_str()
            .ok_or("SRE token renewal omitted expiry")?;
        let expiry = DateTime::parse_from_rfc3339(expiry)
            .map_err(|_| "SRE token renewal expiry invalid")?
            .with_timezone(&Utc);
        if expiry <= Utc::now() + chrono::Duration::minutes(5) {
            return Err("SRE token renewal returned an unusable lifetime".into());
        }
        *token = Token {
            value: new_token.into(),
            expiry,
        };
        Ok(token.value.clone())
    }

    async fn metadata_json(&self, path: &str, stage: &'static str) -> Result<Value, String> {
        let response = self
            .client
            .get(format!(
                "{}{}",
                self.config.kube_url.trim_end_matches('/'),
                path
            ))
            .bearer_auth(self.bearer().await?)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(|error| {
                transport_failure(stage, &error);
                "SRE authority read failed"
            })?;
        if !response.status().is_success() {
            denied_response(stage, response.status());
            return Err("SRE authority read denied".into());
        }
        response
            .json()
            .await
            .map_err(|_| "SRE authority response invalid".into())
    }

    pub(super) async fn authorize(&self) -> Result<(), String> {
        let reg = self
            .metadata_json(
                "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical",
                "registration",
            )
            .await?;
        if reg["metadata"]["uid"] != self.config.registration_uid
            || !reg["metadata"]["deletionTimestamp"].is_null()
            || reg["spec"]["enabled"] != true
            || reg["status"]["phase"] != "Ready"
            || reg["status"]["observedGeneration"] != reg["metadata"]["generation"]
            || reg["status"]["privacyEpoch"] != self.config.privacy_epoch
            || reg["status"]["legacySecretAccessDenied"] != true
            || reg["status"]["privacyRevision"] != crate::sre_privacy::REVISION
            || reg["spec"]["sandbox"]["uid"] != self.config.source.uid
            || reg["spec"]["sandbox"]["namespace"] != self.config.source.namespace
            || reg["spec"]["runtimeNamespace"]["uid"] != self.config.namespace_uid
        {
            return Err("SRE authority is no longer current".into());
        }
        let namespace = self
            .metadata_json(
                &format!("/api/v1/namespaces/{}", self.config.runtime_namespace),
                "namespace",
            )
            .await?;
        let annotations = &namespace["metadata"]["annotations"];
        if namespace["metadata"]["uid"] != self.config.namespace_uid
            || !namespace["metadata"]["deletionTimestamp"].is_null()
            || namespace["metadata"]["ownerReferences"]
                .as_array()
                .is_some_and(|refs| !refs.is_empty())
            || annotations["kars.azure.com/namespace-claim-version"] != "v1"
            || annotations["kars.azure.com/sandbox-namespace"] != self.config.source.namespace
            || annotations["kars.azure.com/sandbox-name"] != self.config.source.name
            || annotations["kars.azure.com/sandbox-uid"] != self.config.source.uid
            || !annotations["kars.azure.com/namespace-prestage"].is_null()
        {
            return Err("SRE runtime namespace ownership changed".into());
        }
        let sandbox = self
            .metadata_json(
                &format!(
                    "/apis/kars.azure.com/v1alpha1/namespaces/{}/karssandboxes/{}",
                    self.config.source.namespace, self.config.source.name
                ),
                "source",
            )
            .await?;
        if sandbox["metadata"]["uid"] != self.config.source.uid
            || !sandbox["metadata"]["deletionTimestamp"].is_null()
            || sandbox["metadata"]["annotations"]["kars.azure.com/namespace-uid"]
                != self.config.namespace_uid
        {
            return Err("SRE source identity changed".into());
        }
        let sa = self
            .metadata_json(
                &format!(
                    "/api/v1/namespaces/{}/serviceaccounts/sre-api-router",
                    self.config.runtime_namespace
                ),
                "service-account",
            )
            .await?;
        if sa["metadata"]["uid"] != self.config.service_account_uid
            || !sa["metadata"]["deletionTimestamp"].is_null()
        {
            return Err("Private SRE ServiceAccount was replaced".into());
        }
        self.verify_privacy().await?;
        Ok(())
    }

    async fn verify_privacy(&self) -> Result<(), String> {
        for review in crate::sre_privacy::secret_access_reviews(&self.config.runtime_namespace) {
            let response = self
                .client
                .post(format!(
                    "{}/apis/authorization.k8s.io/v1/subjectaccessreviews",
                    self.config.kube_url.trim_end_matches('/')
                ))
                .bearer_auth(self.bearer().await?)
                .json(&review)
                .send()
                .await
                .map_err(|error| {
                    transport_failure("privacy-review", &error);
                    "SRE privacy authorization transport failure"
                })?;
            if !response.status().is_success() {
                denied_response("privacy-review", response.status());
                return Err("SRE privacy authorization review denied".into());
            }
            let response: Value = response
                .json()
                .await
                .map_err(|_| "SRE privacy authorization response invalid")?;
            crate::sre_privacy::require_denial(&response)?;
        }
        let response = self
            .client
            .get(format!(
                "{}/api/v1/namespaces/{}/secrets",
                self.config.kube_url.trim_end_matches('/'),
                self.config.runtime_namespace
            ))
            .bearer_auth(self.bearer().await?)
            .header(
                "accept",
                "application/json;as=PartialObjectMetadataList;g=meta.k8s.io;v=v1",
            )
            .send()
            .await
            .map_err(|error| {
                transport_failure("credential-metadata", &error);
                "SRE credential metadata inventory failed"
            })?;
        if !response.status().is_success() {
            denied_response("credential-metadata", response.status());
            return Err("SRE credential metadata inventory denied".into());
        }
        let metadata: Value = response
            .json()
            .await
            .map_err(|_| "SRE credential metadata response invalid")?;
        crate::sre_privacy::reject_legacy_aliases(
            &metadata,
            &[self.config.service_account_uid.as_str()],
        )?;
        Ok(())
    }

    pub(super) async fn forward(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        logs: bool,
    ) -> Result<reqwest::Response, String> {
        self.authorize().await?;
        let mut request = self
            .client
            .request(
                method,
                format!("{}{}", self.config.kube_url.trim_end_matches('/'), path),
            )
            .bearer_auth(self.bearer().await?)
            .header("accept", if logs { "*/*" } else { "application/json" });
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
            .send()
            .await
            .map_err(|_| "Kubernetes diagnostic transport failure".into())
    }

    pub(super) fn renew_in_background(self: &Arc<Self>) {
        let backend = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if backend.bearer().await.is_err() {
                    tracing::warn!("Private SRE token renewal unavailable; requests fail closed");
                }
            }
        });
    }
}
