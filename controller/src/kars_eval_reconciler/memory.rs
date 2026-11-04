// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Bind memory evaluations to the target's currently enforced store, not a guessed name.

use anyhow::{Context, ensure};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client, ResourceExt};
use serde::Serialize;
use serde_json::{Value, json};

use super::workloads;
use crate::{
    crd::KarsSandbox,
    kars_eval::KarsEval,
    kars_memory::KarsMemory,
    kars_memory_compile::{MEMORY_BINDING_FILENAME, compile_to_binding, kars_memory_digest},
    memory_access::MemoryAccess,
    providers::signing::content_digest,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Target {
    pub sandbox_uid: String,
    memory_uid: String,
    memory_generation: i64,
    memory_spec_digest: String,
    configmap_uid: String,
    binding_digest: String,
    store: String,
}

impl Target {
    pub async fn resolve(client: &Client, eval: &KarsEval) -> anyhow::Result<Self> {
        let namespace = workloads::namespace(eval)?;
        let sandbox = Api::<KarsSandbox>::namespaced(client.clone(), namespace)
            .get(&eval.spec.target_sandbox_ref.name)
            .await?;
        let reference = sandbox
            .spec
            .memory_ref
            .as_ref()
            .context("memory evaluation requires memoryRef")?;
        let memory = Api::<KarsMemory>::namespaced(client.clone(), namespace)
            .get(&reference.name)
            .await?;
        let configmap = Api::<ConfigMap>::namespaced(client.clone(), namespace)
            .get(&format!("karsmemory-{}-binding", memory.name_any()))
            .await?;
        Self::from_resources(eval, &sandbox, &memory, &configmap)
    }

    fn from_resources(
        eval: &KarsEval,
        sandbox: &KarsSandbox,
        memory: &KarsMemory,
        configmap: &ConfigMap,
    ) -> anyhow::Result<Self> {
        let namespace = workloads::namespace(eval)?;
        let reference = sandbox
            .spec
            .memory_ref
            .as_ref()
            .context("memory evaluation requires memoryRef")?;
        let generation = memory
            .metadata
            .generation
            .filter(|g| *g > 0)
            .context("memory generation missing")?;
        let status = memory
            .status
            .as_ref()
            .context("memory binding has no observed status")?;
        let configmap_name = format!("karsmemory-{}-binding", memory.name_any());
        ensure!(
            sandbox.namespace().as_deref() == Some(namespace)
                && sandbox.name_any() == eval.spec.target_sandbox_ref.name
                && sandbox.metadata.deletion_timestamp.is_none()
                && memory.namespace().as_deref() == Some(namespace)
                && memory.name_any() == reference.name
                && memory.spec.sandbox_ref.name == sandbox.name_any()
                && memory.metadata.deletion_timestamp.is_none(),
            "memory evaluation target or binding ownership changed"
        );
        ensure!(
            status.phase.as_deref() == Some("Ready")
                && status.observed_generation == Some(generation)
                && status
                    .binding_config_map_ref
                    .as_ref()
                    .map(|r| r.name.as_str())
                    == Some(configmap_name.as_str())
                && configmap.namespace().as_deref() == Some(namespace)
                && configmap.name_any() == configmap_name
                && configmap.metadata.deletion_timestamp.is_none()
                && configmap.labels().get("kars.azure.com/karsmemory") == Some(&memory.name_any()),
            "memory evaluation requires a current Ready binding and its published ConfigMap"
        );
        let data = configmap
            .data
            .as_ref()
            .context("memory binding data missing")?;
        ensure!(
            data.len() == 1 && configmap.binary_data.as_ref().is_none_or(|d| d.is_empty()),
            "memory binding is ambiguous"
        );
        let body = data
            .get(MEMORY_BINDING_FILENAME)
            .context("memory binding.json missing")?;
        let digest = kars_memory_digest(body.as_bytes());
        ensure!(
            status.compiled_digest.as_ref() == Some(&digest)
                && status.loaded_digest.as_ref() == Some(&digest)
                && configmap
                    .annotations()
                    .get("kars.azure.com/claw-memory-digest")
                    == Some(&digest),
            "memory binding is not confirmed loaded by the target router"
        );
        let binding: Value = serde_json::from_str(body)?;
        ensure!(
            binding["sandboxRef"]["name"].as_str() == Some(sandbox.name_any().as_str()),
            "compiled memory target differs"
        );
        let access: MemoryAccess = serde_json::from_value(binding.clone())?;
        access.validate().map_err(anyhow::Error::msg)?;
        if let Some(bundle) = &memory.spec.bundle_ref {
            ensure!(
                status.bundle_ref_digest.as_ref() == Some(&bundle.digest),
                "memory signed bundle observation is stale"
            );
        } else {
            ensure!(
                binding == compile_to_binding(&memory.spec),
                "memory binding differs from current inline spec"
            );
        }
        Ok(Self {
            sandbox_uid: workloads::identity(&sandbox.metadata)?.0.into(),
            memory_uid: workloads::identity(&memory.metadata)?.0.into(),
            memory_generation: generation,
            memory_spec_digest: content_digest(&serde_json::to_vec(&memory.spec)?),
            configmap_uid: workloads::identity(&configmap.metadata)?.0.into(),
            binding_digest: digest,
            store: access.store_name,
        })
    }

    pub fn configure(&self, spec: &mut Value) -> anyhow::Result<()> {
        let args = spec["containers"][0]["args"]
            .as_array_mut()
            .context("runner arguments missing")?;
        ensure!(
            !args
                .iter()
                .any(|arg| arg.as_str() == Some("--memory-store")),
            "memory store already configured"
        );
        args.extend([json!("--memory-store"), json!(self.store)]);
        Ok(())
    }

    pub async fn validate(&self, client: &Client, eval: &KarsEval) -> anyhow::Result<()> {
        ensure!(
            &Self::resolve(client, eval).await? == self,
            "memory evaluation binding identity or content changed"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (KarsEval, KarsSandbox, KarsMemory, ConfigMap) {
        let eval: KarsEval = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsEval",
            "metadata":{"name":"eval","namespace":"control","uid":"eval-uid","resourceVersion":"1","generation":1},
            "spec":{"targetSandboxRef":{"name":"demo"},"corpus":{"builtin":"memory-isolation"}}
        })).unwrap();
        let mut sandbox = KarsSandbox::new("demo", Default::default());
        sandbox.metadata.namespace = Some("control".into());
        sandbox.metadata.uid = Some("sandbox-uid".into());
        sandbox.metadata.resource_version = Some("1".into());
        sandbox.spec.memory_ref = Some(crate::mcp_server::LocalObjectRef {
            name: "binding".into(),
        });
        let mut memory = KarsMemory::new(
            "binding",
            crate::kars_memory::KarsMemorySpec {
                store_name: Some("actual-store".into()),
                scope: Some("private".into()),
                sandbox_ref: crate::kars_memory::SandboxRef {
                    name: "demo".into(),
                },
                ..Default::default()
            },
        );
        memory.metadata.namespace = Some("control".into());
        memory.metadata.uid = Some("memory-uid".into());
        memory.metadata.resource_version = Some("2".into());
        memory.metadata.generation = Some(1);
        let body = serde_json::to_string(&compile_to_binding(&memory.spec)).unwrap();
        let digest = kars_memory_digest(body.as_bytes());
        memory.status = Some(serde_json::from_value(json!({
            "phase":"Ready","observedGeneration":1,"compiledDigest":digest,"loadedDigest":digest,
            "bindingConfigMapRef":{"name":"karsmemory-binding-binding"}
        })).unwrap());
        let configmap = serde_json::from_value(json!({
            "apiVersion":"v1","kind":"ConfigMap",
            "metadata":{"name":"karsmemory-binding-binding","namespace":"control","uid":"cm-uid","resourceVersion":"3",
                "labels":{"kars.azure.com/karsmemory":"binding"},"annotations":{"kars.azure.com/claw-memory-digest":digest}},
            "data":{"binding.json":body}
        })).unwrap();
        (eval, sandbox, memory, configmap)
    }

    #[test]
    fn memory_evaluation_uses_actual_store_without_new_authority() {
        let (eval, sandbox, memory, configmap) = fixture();
        let target = Target::from_resources(&eval, &sandbox, &memory, &configmap).unwrap();
        let mut pod = super::super::runner::runner_pod_spec_json(
            "eval",
            "corpus",
            "runner:latest",
            "http://router:8443",
            "",
        );
        let before = pod.clone();
        target.configure(&mut pod).unwrap();
        assert_eq!(
            pod["containers"][0]["args"].as_array().unwrap().last(),
            Some(&json!("actual-store"))
        );
        pod["containers"][0]["args"] = before["containers"][0]["args"].clone();
        assert_eq!(pod, before);
        let mut recreated = memory.clone();
        recreated.metadata.uid = Some("replacement".into());
        assert_ne!(
            target,
            Target::from_resources(&eval, &sandbox, &recreated, &configmap).unwrap()
        );
    }

    #[tokio::test]
    async fn memory_evaluation_intent_revalidates_binding_before_result_adoption() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let (eval, sandbox, memory, mut cm) = fixture();
        let server = MockServer::start().await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let routes = |cm: &ConfigMap| {
            [
                (
                    "/apis/kars.azure.com/v1alpha1/namespaces/control/karsevals/eval",
                    json!(eval),
                ),
                (
                    "/apis/kars.azure.com/v1alpha1/namespaces/control/karssandboxes/demo",
                    json!(sandbox),
                ),
                (
                    "/apis/kars.azure.com/v1alpha1/namespaces/control/karsmemories/binding",
                    json!(memory),
                ),
                (
                    "/api/v1/namespaces/control/configmaps/karsmemory-binding-binding",
                    json!(cm),
                ),
            ]
        };
        for (uri, body) in routes(&cm) {
            Mock::given(method("GET"))
                .and(path(uri))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        let base = workloads::Intent::new(
            &client,
            &eval,
            "corpus-digest",
            "runner:latest",
            "http://router:8443",
        )
        .await
        .unwrap();
        let target = Target::resolve(&client, &eval).await.unwrap();
        let intent = base.clone().with_memory(target.clone()).unwrap();
        assert_ne!(intent.digest, base.digest);
        intent.revalidate(&client, &eval).await.unwrap();
        let mut wrong_target = target;
        wrong_target.sandbox_uid = "replacement".into();
        assert!(base.clone().with_memory(wrong_target).is_err());

        server.reset().await;
        cm.metadata.uid = Some("replacement".into());
        for (uri, body) in routes(&cm) {
            Mock::given(method("GET"))
                .and(path(uri))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        assert!(intent.revalidate(&client, &eval).await.is_err());
        let current = base
            .with_memory(Target::resolve(&client, &eval).await.unwrap())
            .unwrap();
        assert_ne!(intent.digest, current.digest);
    }

    #[test]
    fn memory_evaluation_rejects_stale_foreign_or_unavailable_bindings() {
        for mutation in [
            "missing-ref",
            "foreign-sandbox",
            "foreign-memory",
            "foreign-cm",
            "stale-generation",
            "not-ready",
            "unloaded",
            "digest",
            "data",
            "ambiguous",
            "inline-spec",
            "missing-uid",
            "terminating",
        ] {
            let (eval, mut sandbox, mut memory, mut cm) = fixture();
            match mutation {
                "missing-ref" => sandbox.spec.memory_ref = None,
                "foreign-sandbox" => memory.spec.sandbox_ref.name = "other".into(),
                "foreign-memory" => memory.metadata.name = Some("other".into()),
                "foreign-cm" => cm.metadata.namespace = Some("other".into()),
                "stale-generation" => memory.metadata.generation = Some(2),
                "not-ready" => memory.status.as_mut().unwrap().phase = Some("Compiled".into()),
                "unloaded" => memory.status.as_mut().unwrap().loaded_digest = None,
                "digest" => {
                    memory.status.as_mut().unwrap().compiled_digest = Some("different".into())
                }
                "data" => {
                    cm.data
                        .as_mut()
                        .unwrap()
                        .insert("binding.json".into(), "{}".into());
                }
                "ambiguous" => {
                    cm.data
                        .as_mut()
                        .unwrap()
                        .insert("other.json".into(), "{}".into());
                }
                "inline-spec" => memory.spec.shared_scopes = Some(vec!["team".into()]),
                "missing-uid" => cm.metadata.uid = None,
                "terminating" => {
                    memory.metadata.deletion_timestamp =
                        Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                            k8s_openapi::jiff::Timestamp::now(),
                        ))
                }
                _ => unreachable!(),
            }
            assert!(
                Target::from_resources(&eval, &sandbox, &memory, &cm).is_err(),
                "{mutation}"
            );
        }
    }
}
