use super::Cluster;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::{Api, ListParams};

impl Cluster {
    // ── Bridge engineering intake sources ───────────────────────────────────

    pub async fn read_engineering_source(
        &self,
        name: &str,
    ) -> Result<Option<ConfigMap>, kube::Error> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        api.get_opt(name).await
    }

    pub async fn list_engineering_sources(
        &self,
        limit: u32,
    ) -> Result<Vec<ConfigMap>, kube::Error> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let params = ListParams::default()
            .labels("bridge.kars.azure.com/engineering-source=true")
            .limit(limit);
        Ok(api.list(&params).await?.items)
    }

    pub async fn create_engineering_source(
        &self,
        name: &str,
        annotations: &std::collections::BTreeMap<String, String>,
        data: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), kube::Error> {
        use kube::api::PostParams;
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let config_map: ConfigMap = serde_json::from_value(serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": name,
                "namespace": "kars-system",
                "labels": {
                    "app.kubernetes.io/managed-by": "kars-bridge",
                    "bridge.kars.azure.com/engineering-source": "true",
                },
                "annotations": annotations,
            },
            "data": data,
        }))
        .expect("engineering source ConfigMap is valid");
        api.create(&PostParams::default(), &config_map)
            .await
            .map(|_| ())
    }

    pub async fn patch_engineering_source_data(
        &self,
        name: &str,
        data: &std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        api.patch(
            name,
            &PatchParams::default(),
            &Patch::Merge(serde_json::json!({ "data": data })),
        )
        .await?;
        Ok(())
    }

    pub async fn claim_engineering_source(
        &self,
        name: &str,
        expected_config: &str,
        expected_status: &str,
        claimed_status: &str,
    ) -> Result<bool, kube::Error> {
        use kube::api::{Patch, PatchParams};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let patch = json_patch::Patch(vec![
            json_patch::PatchOperation::Test(json_patch::TestOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "config.json"]),
                value: serde_json::Value::String(expected_config.to_string()),
            }),
            json_patch::PatchOperation::Test(json_patch::TestOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "status.json"]),
                value: serde_json::Value::String(expected_status.to_string()),
            }),
            json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "status.json"]),
                value: serde_json::Value::String(claimed_status.to_string()),
            }),
        ]);
        match api
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Json::<ConfigMap>(patch),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(kube::Error::Api(error))
                if error.code == 404 || error.code == 409 || error.code == 422 =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn complete_engineering_source_claim(
        &self,
        name: &str,
        expected_claimed_status: &str,
        cursor: &str,
        completed_status: &str,
    ) -> Result<bool, kube::Error> {
        use kube::api::{Patch, PatchParams};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let patch = json_patch::Patch(vec![
            json_patch::PatchOperation::Test(json_patch::TestOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "status.json"]),
                value: serde_json::Value::String(expected_claimed_status.to_string()),
            }),
            json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "cursor.json"]),
                value: serde_json::Value::String(cursor.to_string()),
            }),
            json_patch::PatchOperation::Add(json_patch::AddOperation {
                path: json_patch::jsonptr::PointerBuf::from_tokens(["data", "status.json"]),
                value: serde_json::Value::String(completed_status.to_string()),
            }),
        ]);
        match api
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Json::<ConfigMap>(patch),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(kube::Error::Api(error))
                if error.code == 404 || error.code == 409 || error.code == 422 =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn delete_engineering_source(&self, name: &str) -> anyhow::Result<()> {
        use kube::api::DeleteParams;
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        if api.get_opt(name).await?.is_some() {
            api.delete(name, &DeleteParams::default()).await?;
        }
        Ok(())
    }

    pub async fn replace_engineering_source(
        &self,
        mut current: ConfigMap,
        annotations: &std::collections::BTreeMap<String, String>,
        data: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), kube::Error> {
        use kube::api::PostParams;
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        let name = current.metadata.name.clone().unwrap_or_default();
        current.metadata.annotations = Some(annotations.clone());
        current.data = Some(data.clone());
        api.replace(&name, &PostParams::default(), &current)
            .await
            .map(|_| ())
    }

    pub async fn delete_engineering_source_if_version(
        &self,
        name: &str,
        resource_version: String,
    ) -> Result<(), kube::Error> {
        use kube::api::{DeleteParams, Preconditions};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), "kars-system");
        api.delete(
            name,
            &DeleteParams {
                preconditions: Some(Preconditions {
                    resource_version: Some(resource_version),
                    uid: None,
                }),
                ..DeleteParams::default()
            },
        )
        .await
        .map(|_| ())
    }
}
