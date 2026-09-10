// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::store::StoreError;
use crate::inference_budget_contract::{BudgetError, catalog::Catalog};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client, ResourceExt};

pub const AUDIENCE: &str = "kars.azure.com/governed-inference-budget";
pub const PRIVATE_MOUNT: &str = "/var/run/kars/inference-budget";
pub const TOKEN_VOLUME: &str = "kars-inference-budget-token";
pub const CATALOG_KEY: &str = "contracts.json";

#[derive(Clone, Debug)]
pub struct Settings {
    pub accounting_namespace: String,
    pub catalog_name: String,
    pub address: String,
    pub tls_secret: String,
    pub router_image_digest: String,
}

impl Settings {
    /// Absent/false preserves standalone operation and the existing launch
    /// rejection for finite budgets. Enabling is not proof of readiness.
    pub fn from_env() -> Result<Option<Self>, StoreError> {
        let enabled = std::env::var("KARS_INFERENCE_BUDGET_ENABLED").unwrap_or_default();
        match enabled.as_str() {
            "" | "false" => return Ok(None),
            "true" => {}
            _ => return Err(BudgetError::Contract.into()),
        }
        let settings = Self {
            accounting_namespace: crate::providers::signing::receipt_namespace(),
            catalog_name: std::env::var("KARS_INFERENCE_BUDGET_CATALOG")
                .unwrap_or_else(|_| "kars-inference-budget-contracts".into()),
            address: std::env::var("KARS_INFERENCE_BUDGET_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:9447".into()),
            tls_secret: std::env::var("KARS_INFERENCE_BUDGET_TLS_SECRET")
                .map_err(|_| BudgetError::Contract)?,
            router_image_digest: std::env::var("KARS_INFERENCE_BUDGET_ROUTER_DIGEST")
                .map_err(|_| BudgetError::Contract)?,
        };
        if !crate::inference_budget_contract::valid_label(&settings.accounting_namespace)
            || !crate::inference_budget_contract::valid_name(&settings.catalog_name)
            || !crate::inference_budget_contract::valid_name(&settings.tls_secret)
            || settings.address.parse::<std::net::SocketAddr>().is_err()
            || !crate::inference_budget_contract::valid_digest(&settings.router_image_digest)
        {
            return Err(BudgetError::Contract.into());
        }
        Ok(Some(settings))
    }

    pub async fn catalog(
        &self,
        client: &Client,
        now: i64,
    ) -> Result<ConfiguredCatalog, StoreError> {
        super::admission::verify(client, &self.accounting_namespace).await?;
        let api: Api<ConfigMap> = Api::namespaced(client.clone(), &self.accounting_namespace);
        let value = api
            .get(&self.catalog_name)
            .await
            .map_err(|error| StoreError::Api {
                stage: "read operator inference contracts",
                code: match error {
                    kube::Error::Api(status) => Some(status.code),
                    _ => None,
                },
            })?;
        if value.metadata.uid.as_deref().is_none_or(str::is_empty)
            || value
                .metadata
                .resource_version
                .as_deref()
                .is_none_or(str::is_empty)
            || value.metadata.deletion_timestamp.is_some()
            || value
                .annotations()
                .get("kars.azure.com/inference-budget-contracts")
                .map(String::as_str)
                != Some("v1")
        {
            return Err(BudgetError::Contract.into());
        }
        let encoded = value
            .data
            .as_ref()
            .and_then(|data| data.get(CATALOG_KEY))
            .ok_or(BudgetError::Contract)?;
        if encoded.len() > 262_144 {
            return Err(BudgetError::Capacity.into());
        }
        let catalog: Catalog = serde_json::from_str(encoded).map_err(|_| BudgetError::Contract)?;
        catalog.validate(now)?;
        let mut canonical = serde_json::to_value(&catalog).map_err(|_| BudgetError::Contract)?;
        canonical.sort_all_objects();
        let digest = crate::providers::signing::sha256_hex(
            &serde_json::to_vec(&canonical).map_err(|_| BudgetError::Contract)?,
        );
        Ok(ConfiguredCatalog {
            catalog,
            uid: value.metadata.uid.ok_or(BudgetError::Identity)?,
            resource_version: value
                .metadata
                .resource_version
                .ok_or(BudgetError::Identity)?,
            digest,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ConfiguredCatalog {
    pub catalog: Catalog,
    pub uid: String,
    pub resource_version: String,
    pub digest: String,
}

impl Settings {
    pub async fn public_ca(&self, client: &Client) -> Result<(String, String), StoreError> {
        let api: Api<ConfigMap> = Api::namespaced(client.clone(), &self.accounting_namespace);
        let map = api
            .get(&self.catalog_name)
            .await
            .map_err(|error| StoreError::Api {
                stage: "read budget public CA",
                code: match error {
                    kube::Error::Api(status) => Some(status.code),
                    _ => None,
                },
            })?;
        if map.metadata.uid.is_none()
            || map.metadata.resource_version.is_none()
            || map.metadata.deletion_timestamp.is_some()
            || map
                .annotations()
                .get("kars.azure.com/inference-budget-contracts")
                .map(String::as_str)
                != Some("v1")
        {
            return Err(BudgetError::Contract.into());
        }
        let ca = map
            .data
            .as_ref()
            .and_then(|data| data.get("ca.crt"))
            .ok_or(BudgetError::Contract)?;
        if !ca.contains("-----BEGIN CERTIFICATE-----")
            || ca.contains("PRIVATE KEY")
            || ca.len() > 65_536
        {
            return Err(BudgetError::Contract.into());
        }
        let digest = crate::providers::signing::sha256_hex(ca.as_bytes());
        Ok((ca.clone(), digest))
    }
}
