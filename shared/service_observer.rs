// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CAPABILITY: &str = "kars.azure.com/egress-observation/v1";
pub const SECRET: &str = "router-services-observer";
pub const TOKEN_KEY: &str = "observation-token";
pub const DIRECTORY: &str = "/etc/kars/observations";
pub const VERSION_ENV: &str = "KARS_SERVICE_OBSERVATION_VERSION";
pub const STATUS_FIELD: &str = "serviceObservation";
pub const TLS_SECRET: &str = "router-services-observer-identity";
pub const TLS_DIRECTORY: &str = "/etc/kars/observation-identity";
pub const PORT: u16 = 9447;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recipient {
    pub namespace: String,
    pub namespace_uid: String,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub namespace: String,
    pub name: String,
    pub uid: String,
    pub generation: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    pub capability: String,
    pub identity: Value,
    pub grant: Grant,
    pub recipients: Vec<Recipient>,
    pub privacy_revision: String,
    pub privacy_epoch: Option<String>,
    pub server_name: String,
    pub ca_pem: String,
    #[serde(default)]
    pub workspace_uid: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub verifier: Option<crate::observation_privacy::Endpoint>,
}

impl Binding {
    pub fn valid(&self) -> bool {
        let name = |value: &str, max: usize| {
            !value.is_empty()
                && value.len() <= max
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-.".contains(&byte))
        };
        self.capability == CAPABILITY
            && name(&self.grant.namespace, 63)
            && self.grant.name == "workspace"
            && name(&self.grant.uid, 128)
            && self.grant.generation > 0
            && !self.recipients.is_empty()
            && self.recipients.len() <= 16
            && self.recipients.iter().all(|recipient| {
                name(&recipient.namespace, 63)
                    && name(&recipient.name, 253)
                    && name(&recipient.uid, 128)
                    && name(&recipient.namespace_uid, 128)
            })
            && self.identity["managed"] == true
            && self.server_name.starts_with("observer-")
            && self.server_name.ends_with(".kars.internal")
            && name(&self.server_name, 253)
            && self.ca_pem.starts_with("-----BEGIN CERTIFICATE-----")
            && name(&self.workspace_uid, 128)
            && self.expires_at > 0
            && self
                .verifier
                .as_ref()
                .is_some_and(|endpoint| endpoint.valid(0))
    }
}
