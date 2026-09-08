// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::api_error;
use crate::sre_registration::{KarsSRERegistration, ROUTER_SA, RUNTIME_NAMESPACE};
use k8s_openapi::api::core::v1::{Secret, ServiceAccount};
use kube::{Api, Client, api::ListParams};

pub(super) async fn scan(client: &Client, reg: &KarsSRERegistration) -> Result<(), String> {
    let account = Api::<ServiceAccount>::namespaced(client.clone(), RUNTIME_NAMESPACE)
        .get_opt(ROUTER_SA)
        .await
        .map_err(|e| api_error("Inspect reserved SRE token identity", e))?;
    let mut uids = Vec::new();
    if let Some(uid) = account.as_ref().and_then(|sa| sa.metadata.uid.as_deref()) {
        uids.push(uid);
    }
    if let Some(uid) = reg
        .status
        .as_ref()
        .and_then(|status| status.router_service_account_uid.as_deref())
    {
        uids.push(uid);
    }
    let metadata = Api::<Secret>::namespaced(client.clone(), RUNTIME_NAMESPACE)
        .list_metadata(&ListParams::default())
        .await
        .map_err(|e| api_error("Inspect prestaged SRE token Secret metadata", e))?;
    let list = serde_json::to_value(metadata)
        .map_err(|_| "SRE token metadata inventory could not be decoded")?;
    crate::sre_privacy::reject_legacy_aliases(&list, &uids).map_err(str::to_owned)
}
