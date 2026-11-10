// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::api_error;
use crate::{
    sre_registration::KarsSRERegistration,
    sre_report::{Identity, NAME, NAMESPACE, SOURCE},
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client, api::PostParams};
use serde_json::Value;

fn reconcile_target(identity: &Identity, existing: Option<Value>) -> Result<Option<Value>, String> {
    let Some(mut target) = existing else {
        return Ok(Some(identity.target()));
    };
    if !identity.owns_target(&target) {
        return Err("SRE report target is not owned by the live registration; no adoption".into());
    }
    if identity.matches_target(&target) {
        return Ok(None);
    }
    // Source replacement rotates the binding and clears the prior source's report in one CAS.
    target["metadata"]["annotations"][SOURCE] =
        identity.target()["metadata"]["annotations"][SOURCE].clone();
    target["data"] = serde_json::json!({});
    Ok(Some(target))
}

pub(super) async fn ensure(client: &Client, reg: &KarsSRERegistration) -> Result<(), String> {
    let identity = Identity::from_registration(
        &serde_json::to_value(reg).map_err(|_| "Report identity invalid")?,
    )
    .ok_or("Report requires the canonical registered SRE source")?;
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), NAMESPACE);
    let existing = api
        .get_opt(NAME)
        .await
        .map_err(|e| api_error("Read SRE report target", e))?;
    let exists = existing.is_some();
    let existing = existing
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| "Report target invalid")?;
    if let Some(target) = reconcile_target(&identity, existing)? {
        super::live::verify(client, reg).await?;
        let target: ConfigMap =
            serde_json::from_value(target).map_err(|_| "Report target invalid")?;
        if exists {
            api.replace(NAME, &PostParams::default(), &target)
                .await
                .map_err(|e| api_error("Rotate SRE report source", e))?;
        } else {
            api.create(&PostParams::default(), &target)
                .await
                .map_err(|e| api_error("Create SRE report target", e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn report_target_survives_pod_replacement_and_only_rotates_for_registered_source() {
        let mut identity = Identity {
            registration_uid: "reg".into(),
            source_namespace: "kars-system".into(),
            source_uid: "source".into(),
            namespace_uid: "ns".into(),
        };
        let mut target = reconcile_target(&identity, None).unwrap().unwrap();
        target["metadata"]["uid"] = json!("cm-uid");
        target["metadata"]["resourceVersion"] = json!("42");
        target["data"] = json!({"report.json":"prior report"});
        assert!(
            reconcile_target(&identity, Some(target.clone()))
                .unwrap()
                .is_none()
        );
        identity.source_uid = "replacement".into();
        let rotated = reconcile_target(&identity, Some(target.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(rotated["data"], json!({}));
        assert_eq!(rotated["metadata"]["resourceVersion"], "42");
        assert_eq!(rotated["metadata"]["uid"], "cm-uid");
        assert!(identity.matches_target(&rotated));
        identity.registration_uid = "replacement-registration".into();
        assert!(reconcile_target(&identity, Some(target)).is_err());
    }
}
