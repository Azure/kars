// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{cluster::Cluster, credential_contract::Identity};
use kube::{
    Api, ResourceExt,
    api::DynamicObject,
    core::{ApiResource, GroupVersionKind},
};

pub fn failure(message: &str) -> kube::Error {
    kube::Error::Api(
        kube::core::Status {
            status: Some(kube::core::response::StatusSummary::Failure),
            reason: "CredentialAuthorityUnavailable".into(),
            message: message.into(),
            code: 409,
            ..Default::default()
        }
        .boxed(),
    )
}

pub(super) fn safe(stage: &str, error: kube::Error) -> kube::Error {
    let code = if let kube::Error::Api(ref error) = error {
        error.code
    } else {
        502
    };
    kube::Error::Api(
        kube::core::Status {
            status: Some(kube::core::response::StatusSummary::Failure),
            reason: "CredentialOperationFailed".into(),
            message: format!("{stage}: Kubernetes status {code}"),
            code,
            ..Default::default()
        }
        .boxed(),
    )
}

pub(super) fn object_api(cluster: &Cluster, namespace: &str, kind: &str) -> Api<DynamicObject> {
    Api::namespaced_with(
        cluster.client.clone(),
        namespace,
        &ApiResource::from_gvk(&GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind)),
    )
}

pub(super) fn identity(object: &DynamicObject) -> Result<Identity, kube::Error> {
    Ok(Identity {
        name: object.name_any(),
        uid: object
            .uid()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| failure("API target UID missing"))?,
    })
}
