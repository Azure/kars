// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::time::Duration;

const MAX_JWKS_BYTES: usize = 256 * 1024;

#[async_trait::async_trait]
pub(super) trait JwksFetcher: Send + Sync + std::fmt::Debug {
    async fn fetch(&self, issuer: &str) -> Result<FetchedJwks, FetchError>;
}

#[derive(Debug, Clone)]
pub(super) struct FetchedJwks {
    pub raw: Vec<u8>,
    pub key_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum FetchError {
    #[error("issuer discovery: {class}: {detail}")]
    Discovery { class: &'static str, detail: String },
    #[error("JWKS fetch: {class}: {detail}")]
    Jwks { class: &'static str, detail: String },
    #[error("JWKS payload not a JWKSet: {0}")]
    InvalidJwks(String),
}

impl FetchError {
    pub(super) fn class(&self) -> &'static str {
        match self {
            Self::Discovery { class, .. } | Self::Jwks { class, .. } => class,
            Self::InvalidJwks(_) => "invalid_jwks_format",
        }
    }
}

#[derive(Debug)]
pub(super) struct HttpJwksFetcher {
    client: reqwest::Client,
}

impl HttpJwksFetcher {
    pub(super) fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(super::HTTP_TIMEOUT_SECS))
                .https_only(true)
                .build()
                .expect("JWKS client builder"),
        }
    }
}

fn transport_class(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "dns"
    } else {
        "tls"
    }
}

#[async_trait::async_trait]
impl JwksFetcher for HttpJwksFetcher {
    async fn fetch(&self, issuer: &str) -> Result<FetchedJwks, FetchError> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let response =
            self.client
                .get(url)
                .send()
                .await
                .map_err(|error| FetchError::Discovery {
                    class: transport_class(&error),
                    detail: "transport failed".into(),
                })?;
        if !response.status().is_success() {
            return Err(FetchError::Discovery {
                class: "http_status",
                detail: response.status().to_string(),
            });
        }
        let discovery: serde_json::Value =
            response.json().await.map_err(|_| FetchError::Discovery {
                class: "invalid_jwks_format",
                detail: "invalid discovery JSON".into(),
            })?;
        let jwks_uri = discovery["jwks_uri"]
            .as_str()
            .ok_or_else(|| FetchError::Discovery {
                class: "invalid_jwks_format",
                detail: "discovery document missing jwks_uri".into(),
            })?
            .to_string();
        let response =
            self.client
                .get(&jwks_uri)
                .send()
                .await
                .map_err(|error| FetchError::Jwks {
                    class: transport_class(&error),
                    detail: "transport failed".into(),
                })?;
        if !response.status().is_success() {
            return Err(FetchError::Jwks {
                class: "http_status",
                detail: response.status().to_string(),
            });
        }
        let bytes = response.bytes().await.map_err(|_| FetchError::Jwks {
            class: "tls",
            detail: "response transport failed".into(),
        })?;
        if bytes.len() > MAX_JWKS_BYTES {
            return Err(FetchError::InvalidJwks(format!(
                "JWKS exceeds {MAX_JWKS_BYTES} bytes"
            )));
        }
        let raw = bytes.to_vec();
        let key_count = parse_jwks_key_count(&raw)?;
        Ok(FetchedJwks { raw, key_count })
    }
}

pub(super) fn parse_jwks_key_count(raw: &[u8]) -> Result<usize, FetchError> {
    let value: serde_json::Value =
        serde_json::from_slice(raw).map_err(|_| FetchError::InvalidJwks("not JSON".into()))?;
    value["keys"]
        .as_array()
        .map(Vec::len)
        .ok_or_else(|| FetchError::InvalidJwks("missing or non-array keys".into()))
}
