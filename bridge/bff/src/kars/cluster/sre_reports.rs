// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::Cluster;
use crate::sre_report::{Identity, NAMESPACE};
use k8s_openapi::api::core::v1::Namespace;
use kube::{
    Api,
    api::{DynamicObject, GroupVersionKind},
    core::ApiResource,
};
use serde_json::Value;

impl Cluster {
    pub(crate) async fn sre_registration(&self) -> Result<Option<Value>, kube::Error> {
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "kars.azure.com",
            "v1alpha1",
            "KarsSRERegistration",
        ));
        let api: Api<DynamicObject> = Api::all_with(self.client.clone(), &resource);
        api.get_opt("canonical")
            .await
            .map(|v| v.map(|v| serde_json::to_value(v).expect("Kubernetes object serializes")))
    }

    pub(crate) async fn sre_report_source_current(
        &self,
        identity: &Identity,
    ) -> Result<bool, kube::Error> {
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "kars.azure.com",
            "v1alpha1",
            "KarsSandbox",
        ));
        let api: Api<DynamicObject> =
            Api::namespaced_with(self.client.clone(), &identity.source_namespace, &resource);
        let Some(source) = api.get_opt("sre").await? else {
            return Ok(false);
        };
        let Some(namespace) = Api::<Namespace>::all(self.client.clone())
            .get_opt(NAMESPACE)
            .await?
        else {
            return Ok(false);
        };
        let source = serde_json::to_value(source).expect("Kubernetes object serializes");
        let namespace = serde_json::to_value(namespace).expect("Kubernetes object serializes");
        Ok(source["metadata"]["uid"] == identity.source_uid
            && source["metadata"]["deletionTimestamp"].is_null()
            && source["metadata"]["annotations"]["kars.azure.com/namespace-uid"]
                == identity.namespace_uid
            && namespace["metadata"]["uid"] == identity.namespace_uid
            && namespace["metadata"]["deletionTimestamp"].is_null()
            && namespace["metadata"]["ownerReferences"]
                .as_array()
                .is_none_or(Vec::is_empty)
            && namespace["metadata"]["annotations"]["kars.azure.com/namespace-prestage"].is_null()
            && namespace["metadata"]["annotations"]["kars.azure.com/namespace-claim-version"]
                == "v1"
            && namespace["metadata"]["annotations"]["kars.azure.com/sandbox-namespace"]
                == identity.source_namespace
            && namespace["metadata"]["annotations"]["kars.azure.com/sandbox-name"] == "sre"
            && namespace["metadata"]["annotations"]["kars.azure.com/sandbox-uid"]
                == identity.source_uid)
    }
}
