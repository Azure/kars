// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// Governed credential adapter — controller and Teams integration operations.

use super::*;

const MODEL_DEFAULT: &str = "kars.azure.com/default-model";

impl Cluster {
    pub async fn model_default(&self) -> Result<Option<crate::kars::task::TaskModel>, kube::Error> {
        let Some((_, secret)) = self
            .optional_integration_store(&self.core_namespace(), "kars-inference-providers")
            .await?
        else {
            return Ok(None);
        };
        secret
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(MODEL_DEFAULT))
            .map(|raw| {
                serde_json::from_str(raw).map_err(|_| {
                    failure("Saved model default is malformed; select a configured model again")
                })
            })
            .transpose()
    }

    pub async fn write_model_default(
        &self,
        model: &crate::kars::task::TaskModel,
        expected: &Secret,
    ) -> Result<(), kube::Error> {
        let namespace = self.core_namespace();
        let (_, current) = self
            .integration_store(&namespace, "kars-inference-providers")
            .await?;
        if current.uid() != expected.uid()
            || current.resource_version() != expected.resource_version()
        {
            return Err(kube::Error::Api(kube::core::ErrorResponse {
                status: "Failure".into(), reason: "Conflict".into(), code: 409,
                message: "Provider configuration changed while selecting the default; refresh and try again".into(),
            }));
        }
        let mut annotations = current.metadata.annotations.unwrap_or_default();
        annotations.insert(
            MODEL_DEFAULT.into(),
            serde_json::to_string(model).map_err(|_| failure("Model default encoding failed"))?,
        );
        let patch: json_patch::Patch = serde_json::from_value(json!([
            {"op":"test","path":"/metadata/uid","value":expected.uid()},
            {"op":"test","path":"/metadata/resourceVersion","value":expected.resource_version()},
            {"op":"add","path":"/metadata/annotations","value":annotations}
        ]))
        .map_err(|_| failure("Model default patch encoding failed"))?;
        Api::<Secret>::namespaced(self.client.clone(), &namespace)
            .patch(
                "kars-inference-providers",
                &PatchParams::default(),
                &Patch::Json::<Secret>(patch),
            )
            .await
            .map_err(|error| safe("Save enrolled model default", error))?;
        Ok(())
    }

    pub async fn require_controller_environment(&self) -> Result<Grant, kube::Error> {
        let namespace = self.core_namespace();
        let grant = self.credential_grant(&namespace).await?;
        if grant.document.data["spec"]["controller"]["name"] != "kars-controller"
            || grant.document.data["spec"]["controller"]["uid"]
                .as_str()
                .is_none_or(str::is_empty)
        {
            return Err(failure(
                "The controller Deployment UID must be enrolled before provider configuration",
            ));
        }
        let workspace = Api::<Namespace>::all(self.client.clone())
            .get(&namespace)
            .await
            .map_err(|error| safe("Recheck controller settings workspace", error))?;
        if workspace.metadata.deletion_timestamp.is_some()
            || grant.document.data["spec"]["workspaceUid"] != json!(workspace.metadata.uid)
        {
            return Err(failure("Controller settings workspace identity changed"));
        }
        if workspace
            .metadata
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get("kars.azure.com/private-enabled"))
            .is_some_and(|enabled| enabled == "true")
        {
            return Err(failure(
                "Controller defaults cannot be changed on a sealed private workspace. No controller settings were saved. Configure defaults before private activation; per-agent model selection is separate.",
            ));
        }
        Ok(grant)
    }

    pub async fn write_controller_environment(
        &self,
        changes: Vec<Value>,
    ) -> Result<(), kube::Error> {
        let namespace = self.core_namespace();
        let grant = self.require_controller_environment().await?;
        let mut incoming = Vec::new();
        for entry in changes {
            let name = entry["name"]
                .as_str()
                .ok_or_else(|| failure("Controller setting name missing"))?;
            if entry["$patch"] == "delete" {
                incoming.push(json!({"name":name,"remove":true}));
            } else if let Some(secret) = entry.get("valueFrom").and_then(|v| v.get("secretKeyRef"))
            {
                let secret_name = secret["name"]
                    .as_str()
                    .ok_or_else(|| failure("Controller Secret reference name missing"))?;
                let store = grant
                    .stores
                    .iter()
                    .find(|store| store.secret.name == secret_name)
                    .ok_or_else(|| failure("Controller credential Secret is not enrolled"))?;
                incoming.push(json!({"name":name,"secret":{"name":secret_name,"uid":store.secret.uid,"key":secret["key"]}}));
            } else if let Some(value) = entry["value"].as_str() {
                incoming.push(json!({"name":name,"value":value}));
            } else {
                return Err(failure("Unsupported controller environment change"));
            }
        }
        self.mutate_integration(&namespace, "kars-credential-controller-settings", |keys| {
            let mut values = keys
                .get("configuration")
                .and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok())
                .unwrap_or_default();
            for change in &incoming {
                values.retain(|existing| existing["name"] != change["name"]);
                values.push(change.clone());
            }
            keys.insert(
                "configuration".into(),
                serde_json::to_string(&values).expect("environment settings serialize"),
            );
        })
        .await
    }

    pub async fn request_teams_reconcile(
        &self,
        namespace: &str,
        gateway: &str,
        bff: &str,
    ) -> Result<(), kube::Error> {
        let grant = self.credential_grant(namespace).await?;
        let consumers = &grant.document.data["spec"]["bridgeConsumers"];
        if consumers["gateway"]["name"] != gateway || consumers["bff"]["name"] != bff {
            return Err(failure(
                "Teams Deployment identities must be enrolled; Bridge cannot patch arbitrary Deployments",
            ));
        }
        if let Some(error) = grant.document.data["status"]["integrationError"].as_str() {
            return Err(failure(error));
        }
        Ok(())
    }

    pub async fn teams_configured(&self) -> Result<bool, kube::Error> {
        let namespace = self.integration_namespace();
        let name = std::env::var("BRIDGE_TEAMS_SECRET_NAME")
            .unwrap_or_else(|_| "kars-bridge-teams".into());
        let Some(document) = object_api(self, &namespace, "KarsCredentialGrant")
            .get_opt(GRANT)
            .await
            .map_err(|e| safe("Read optional Teams authority", e))?
        else {
            return Ok(false);
        };
        if !document.data["spec"]["integrationStores"]
            .as_array()
            .is_some_and(|stores| {
                stores
                    .iter()
                    .any(|store| store["secret"]["name"] == name && store["purpose"] == "teams")
            })
        {
            return Ok(false);
        }
        let (_, secret) = self.integration_store(&namespace, &name).await?;
        Ok([
            "client-id",
            "tenant-id",
            "client-secret",
            "entra-role-map",
            "bff-internal-secret",
        ]
        .iter()
        .all(|key| {
            secret
                .data
                .as_ref()
                .and_then(|values| values.get(*key))
                .is_some_and(|value| !value.0.is_empty())
        }))
    }
}
