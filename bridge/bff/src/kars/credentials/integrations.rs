// Governed credential adapter — controller and Teams integration operations.

use super::*;

impl Cluster {
    pub async fn write_controller_environment(
        &self,
        changes: Vec<Value>,
    ) -> Result<(), kube::Error> {
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
