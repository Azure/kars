// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Purpose-limited observations with live target, grant and recipient identity checks.

use crate::{access_request::Scope, service_observer::*};
use k8s_openapi::api::{
    authorization::v1::SubjectAccessReview,
    core::v1::{Namespace, ServiceAccount},
};
use kube::{
    Api, Client, ResourceExt,
    api::PostParams,
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::json;
use std::{path::Path, sync::Arc};
use tokio::sync::OnceCell;

#[path = "service_observation_client.rs"]
mod client_diagnostics;

pub struct Observer {
    binding: Binding,
    token: String,
    version: String,
    client: OnceCell<Client>,
}

impl Observer {
    pub fn load() -> Result<Option<Arc<Self>>, String> {
        let Ok(version) = std::env::var(VERSION_ENV) else {
            return Ok(None);
        };
        let directory = Path::new(DIRECTORY);
        let binding: Binding = serde_json::from_slice(
            &std::fs::read(directory.join("config.json"))
                .map_err(|_| "Observation binding is unavailable")?,
        )
        .map_err(|_| "Observation binding is invalid")?;
        let token = std::fs::read_to_string(directory.join(TOKEN_KEY))
            .map_err(|_| "Observation credential is unavailable")?;
        if !binding.valid()
            || token.len() != 64
            || !token.bytes().all(|byte| byte.is_ascii_graphic())
            || version.is_empty()
            || version.len() > 256
        {
            return Err("Observation identity or credential is invalid".into());
        }
        Ok(Some(Arc::new(Self {
            binding,
            token,
            version,
            client: OnceCell::new(),
        })))
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        binding: Binding,
        token: String,
        version: String,
        client: Client,
    ) -> Arc<Self> {
        Arc::new(Self {
            binding,
            token,
            version,
            client: OnceCell::from(client),
        })
    }

    pub fn recognizes(&self, provided: Option<&str>) -> bool {
        provided.is_some_and(|provided| {
            crate::handoff::constant_time_eq(self.token.as_bytes(), provided.as_bytes())
        })
    }

    async fn client(&self) -> Result<&Client, String> {
        self.client
            .get_or_try_init(|| async {
                let config = kube::Config::incluster()
                    .map_err(|_| "Observation metadata identity unavailable")?;
                client_diagnostics::client(config)
                    .map_err(|_| "Observation metadata client unavailable".into())
            })
            .await
    }

    pub async fn authorized(
        &self,
        provided: Option<&str>,
        scope: &Scope,
        operation: crate::observation_privacy::Operation,
    ) -> Result<(), String> {
        let mut diagnostic = crate::observation_privacy::Readiness::new("observer_bearer");
        if !self.recognizes(provided) {
            return Err("Observation credential required".into());
        }
        diagnostic.stage("observer_binding");
        if self.binding.expires_at <= chrono::Utc::now().timestamp()
            || self.binding.verifier.is_none()
        {
            return Err("Current private observation verifier capability required".into());
        }
        if serde_json::to_value(&scope.identity).map_err(|_| "Service identity invalid")?
            != self.binding.identity
        {
            return Err("Observation service identity changed".into());
        }
        diagnostic.stage("observer_metadata_client");
        let pending = client_diagnostics::Pending(client_diagnostics::Progress::new(format!(
            "kars-{}",
            scope.identity.sandbox.name
        )));
        let client = self.client().await?;
        pending.0.initialized();
        let namespace = scope.identity.sandbox.namespace.as_str();
        let sandbox_name = scope.identity.sandbox.name.as_str();
        let resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "kars.azure.com",
            "v1alpha1",
            "KarsSandbox",
        ));
        diagnostic.stage("observer_target_read");
        let api = Api::<DynamicObject>::namespaced_with(client.clone(), namespace, &resource);
        let mut request = kube::core::Request::new(api.resource_url())
            .get(sandbox_name, &Default::default())
            .map_err(|_| "Observation target request cannot be built")?;
        request.extensions_mut().insert("get");
        request.extensions_mut().insert(pending.0.clone());
        pending.0.built();
        let sandbox = client_diagnostics::read_target(client, request, &pending.0)
            .await
            .map_err(|error| {
                diagnostic.api(&error);
                "Observation target cannot be verified"
            })?;
        pending.0.decoded();
        diagnostic.stage("observer_target_current");
        let observed = &sandbox.data["status"][STATUS_FIELD];
        if sandbox.metadata.uid.as_deref() != Some(scope.identity.sandbox.uid.as_str())
            || sandbox.metadata.deletion_timestamp.is_some()
            || observed["capability"] != CAPABILITY
            || observed["version"] != self.version
            || !(observed["phase"] == "Ready"
                || (operation == crate::observation_privacy::Operation::Scope
                    && observed["phase"] == "Prepared"))
            || observed["grant"]["uid"] != self.binding.grant.uid
            || observed["namespaceUid"] != scope.identity.namespace_uid
            || observed["privacyRevision"] != self.binding.privacy_revision
            || observed["privacyEpoch"] != json!(self.binding.privacy_epoch)
        {
            return Err("Observation credential is no longer current".into());
        }
        diagnostic.stage("observer_namespace_read");
        let runtime = Api::<Namespace>::all(client.clone())
            .get(&format!("kars-{sandbox_name}"))
            .await
            .map_err(|error| {
                diagnostic.api(&error);
                "Observation namespace cannot be verified"
            })?;
        diagnostic.stage("observer_namespace_current");
        if runtime.uid().as_deref() != Some(scope.identity.namespace_uid.as_str())
            || runtime.metadata.deletion_timestamp.is_some()
        {
            return Err("Observation namespace was replaced".into());
        }
        let grant_resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "kars.azure.com",
            "v1alpha1",
            "KarsCredentialGrant",
        ));
        diagnostic.stage("observer_workspace_read");
        let workspace = Api::<Namespace>::all(client.clone())
            .get(namespace)
            .await
            .map_err(|error| {
                diagnostic.api(&error);
                "Observation workspace cannot be verified"
            })?;
        diagnostic.stage("observer_workspace_current");
        if workspace.uid().as_deref() != Some(self.binding.workspace_uid.as_str())
            || workspace.metadata.deletion_timestamp.is_some()
        {
            return Err("Observation workspace was replaced".into());
        }
        diagnostic.stage("observer_grant_read");
        let grant = Api::<DynamicObject>::namespaced_with(
            client.clone(),
            &self.binding.grant.namespace,
            &grant_resource,
        )
        .get(&self.binding.grant.name)
        .await
        .map_err(|error| {
            diagnostic.api(&error);
            "Observation delegation cannot be verified"
        })?;
        diagnostic.stage("observer_grant_current");
        if grant.uid().as_deref() != Some(self.binding.grant.uid.as_str())
            || grant.metadata.generation != Some(self.binding.grant.generation)
            || grant.metadata.deletion_timestamp.is_some()
            || grant.data["spec"]["enabled"] != true
            || grant.data["spec"]["workspaceUid"] != self.binding.workspace_uid
            || grant.data["status"]["phase"] != "Ready"
            || grant.data["status"]["observedGeneration"] != json!(self.binding.grant.generation)
            || !grant.data["status"]["conditions"]
                .as_array()
                .is_some_and(|conditions| {
                    conditions.iter().any(|condition| {
                        condition["type"] == "WriterReady"
                            && condition["status"] == "True"
                            && condition["observedGeneration"]
                                == json!(self.binding.grant.generation)
                    })
                })
            || !grant.data["spec"]["observationTargets"]
                .as_array()
                .is_some_and(|targets| {
                    targets.iter().any(|target| {
                        target["kind"] == "KarsSandbox"
                            && target["namespace"] == namespace
                            && target["name"] == sandbox_name
                            && target["uid"] == scope.identity.sandbox.uid
                    })
                })
        {
            return Err("Observation delegation changed".into());
        }
        for recipient in &self.binding.recipients {
            diagnostic.stage("observer_recipient_namespace");
            let ns = Api::<Namespace>::all(client.clone())
                .get(&recipient.namespace)
                .await
                .map_err(|error| {
                    diagnostic.api(&error);
                    "Observation recipient namespace cannot be verified"
                })?;
            diagnostic.stage("observer_recipient_account");
            let sa = Api::<ServiceAccount>::namespaced(client.clone(), &recipient.namespace)
                .get(&recipient.name)
                .await
                .map_err(|error| {
                    diagnostic.api(&error);
                    "Observation recipient cannot be verified"
                })?;
            diagnostic.stage("observer_recipient_current");
            if ns.uid().as_deref() != Some(recipient.namespace_uid.as_str())
                || ns.metadata.deletion_timestamp.is_some()
                || sa.uid().as_deref() != Some(recipient.uid.as_str())
                || sa.metadata.deletion_timestamp.is_some()
            {
                return Err("Observation recipient identity was replaced".into());
            }
        }
        diagnostic.stage("observer_privacy_revision");
        if self.binding.privacy_revision != crate::sre_privacy::REVISION {
            return Err("Observation privacy proof version is stale".into());
        }
        let registration_resource = ApiResource::from_gvk(&GroupVersionKind::gvk(
            "kars.azure.com",
            "v1alpha1",
            "KarsSRERegistration",
        ));
        diagnostic.stage("observer_registration_read");
        let registration = Api::<DynamicObject>::all_with(client.clone(), &registration_resource)
            .get_opt("canonical")
            .await
            .map_err(|error| {
                diagnostic.api(&error);
                "Observation privacy authority cannot be read"
            })?;
        diagnostic.stage("observer_registration_current");
        match registration {
            None if self.binding.privacy_epoch.is_none() => {}
            Some(registration) => {
                let current = registration.metadata.deletion_timestamp.is_none()
                    && registration.data["status"]["observedGeneration"]
                        == json!(registration.metadata.generation)
                    && registration.data["status"]["privacyRevision"]
                        == crate::sre_privacy::REVISION
                    && registration.data["status"]["legacySecretAccessDenied"] == true;
                let ready = self
                    .binding
                    .privacy_epoch
                    .as_deref()
                    .is_some_and(|epoch| !epoch.is_empty())
                    && registration.data["spec"]["enabled"] == true
                    && registration.data["status"]["phase"] == "Ready"
                    && registration.data["status"]["privacyEpoch"]
                        == json!(self.binding.privacy_epoch);
                let retired = registration.data["spec"]["enabled"] == false
                    && registration.data["status"]["phase"] == "Retired"
                    && self.binding.privacy_epoch.is_none();
                if !current || !(ready || retired) {
                    return Err("Observation privacy qualification is pending or invalid".into());
                }
            }
            _ => return Err("Observation privacy epoch is no longer current".into()),
        }
        for request in crate::sre_privacy::secret_access_reviews(&runtime.name_any()) {
            diagnostic.stage("observer_secret_denial_review");
            let request: SubjectAccessReview = serde_json::from_value(request)
                .map_err(|_| "Observation privacy request invalid")?;
            let response = Api::<SubjectAccessReview>::all(client.clone())
                .create(&PostParams::default(), &request)
                .await
                .map_err(|error| {
                    diagnostic.api(&error);
                    "Observation privacy authorization unavailable"
                })?;
            diagnostic.stage("observer_secret_denial_result");
            crate::sre_privacy::require_denial(
                &serde_json::to_value(response)
                    .map_err(|_| "Observation privacy response invalid")?,
            )
            .map_err(str::to_string)?;
        }
        diagnostic.stage("observer_verifier");
        crate::observation_privacy_client::verify(
            client,
            &self.binding,
            &self.token,
            &self.version,
            scope,
            operation,
        )
        .await?;
        diagnostic.stage("observer_expiry");
        if self.binding.expires_at <= chrono::Utc::now().timestamp() {
            return Err("Observation credential expired during verification".into());
        }
        diagnostic.finish();
        Ok(())
    }

    pub fn binding(&self) -> &Binding {
        &self.binding
    }
}
