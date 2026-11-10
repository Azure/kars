// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{auth, config::Settings, store::StoreError};
use crate::{
    crd::KarsSandbox,
    inference_budget_contract::{
        BudgetError, ResourceIdentity,
        calendar::{
            ACCOUNT_ANNOTATION, BINDING_ENV, Binding, Commit, Identity, Ledger, Observation,
            POLICY_KEY, Policy,
        },
    },
};
use axum::http::HeaderMap;
use chrono::{Datelike, Utc};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{ConfigMap, Namespace, Pod},
};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::json;

fn api_error(stage: &'static str, error: kube::Error) -> StoreError {
    StoreError::Api {
        stage,
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

pub(crate) const INITIALIZATION_ANNOTATION: &str = "kars.azure.com/calendar-initialization-uid";

fn calendar() -> (u32, u32) {
    let now = Utc::now();
    (
        now.date_naive().num_days_from_ce() as u32,
        now.year() as u32 * 12 + now.month0(),
    )
}

fn name(sandbox: &KarsSandbox) -> Result<String, StoreError> {
    let uid = sandbox.uid().ok_or(BudgetError::Identity)?;
    Ok(format!(
        "calendar-{}",
        crate::providers::signing::sha256_hex(uid.as_bytes())
    ))
}

fn decode(map: &ConfigMap, binding: &Binding) -> Result<Ledger, StoreError> {
    if map.uid().as_deref() != Some(&binding.account.uid)
        || map.metadata.deletion_timestamp.is_some()
        || map.resource_version().is_none()
    {
        return Err(StoreError::Missing);
    }
    let raw = map
        .data
        .as_ref()
        .and_then(|data| data.get("ledger.json"))
        .ok_or(BudgetError::Corrupt)?;
    if raw.len() > 262_144 {
        return Err(BudgetError::Capacity.into());
    }
    let ledger: Ledger = serde_json::from_str(raw).map_err(|_| BudgetError::Corrupt)?;
    ledger.validate(binding)?;
    ledger.validate_orphan_audits(
        map.annotations()
            .get("kars.azure.com/calendar-reconciliations")
            .map(String::as_str),
        &binding.account.uid,
    )?;
    Ok(ledger)
}

pub async fn prepare(
    client: &Client,
    settings: &Settings,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Binding, StoreError> {
    let namespace = crate::reconciler::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| BudgetError::Identity)?;
    let mut binding = Binding {
        sandbox: ResourceIdentity {
            namespace: sandbox.namespace().ok_or(BudgetError::Identity)?,
            name: sandbox.name_any(),
            uid: sandbox.uid().ok_or(BudgetError::Identity)?,
        },
        runtime_namespace_uid: namespace.uid().ok_or(BudgetError::Identity)?,
        account: ResourceIdentity {
            namespace: settings.accounting_namespace.clone(),
            name: name(sandbox)?,
            uid: String::new(),
        },
        privacy_epoch: crate::sre_authority::privacy_epoch(client, &namespace.name_any())
            .await
            .map_err(|_| BudgetError::Authorization)?,
    };
    let maps: Api<ConfigMap> = Api::namespaced(client.clone(), &binding.account.namespace);
    if let Some(uid) = namespace.annotations().get(ACCOUNT_ANNOTATION) {
        if namespace
            .labels()
            .get("kars.azure.com/calendar-budget")
            .map(String::as_str)
            != Some("v1")
        {
            return Err(BudgetError::Identity.into());
        }
        binding.account.uid = uid.clone();
        let map = maps
            .get(&binding.account.name)
            .await
            .map_err(|e| api_error("read sealed calendar account", e))?;
        decode(&map, &binding)?;
        return Ok(binding);
    }
    if namespace.annotations().get(INITIALIZATION_ANNOTATION) != Some(&binding.sandbox.uid) {
        return Err(StoreError::Api {
            stage: "legacy calendar usage requires reconciliation before durable migration",
            code: None,
        });
    }
    // Fresh provenance alone cannot authorize initialization over a workload.
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &namespace.name_any());
    if deployments
        .get_opt(&sandbox.name_any())
        .await
        .map_err(|e| api_error("check calendar migration boundary", e))?
        .is_some()
    {
        return Err(StoreError::Api {
            stage: "legacy calendar usage requires reconciliation before durable migration",
            code: None,
        });
    }
    let (day, month) = calendar();
    let ledger = Ledger::new(
        binding.sandbox.clone(),
        binding.runtime_namespace_uid.clone(),
        day,
        month,
    );
    let value: ConfigMap = serde_json::from_value(json!({
        "metadata": {
            "name": binding.account.name, "namespace": binding.account.namespace,
            "ownerReferences": [{"apiVersion":"v1","kind":"Namespace","name":namespace.name_any(),
                "uid":namespace.uid(),"controller":true,"blockOwnerDeletion":false}]
        },
        "data": {"ledger.json": serde_json::to_string(&ledger).map_err(|_| BudgetError::Corrupt)?}
    }))
    .map_err(|_| BudgetError::Corrupt)?;
    let map = match maps.create(&PostParams::default(), &value).await {
        Ok(map) => map,
        Err(kube::Error::Api(error)) if error.code == 409 => maps
            .get(&binding.account.name)
            .await
            .map_err(|e| api_error("read pending calendar initialization", e))?,
        Err(error) => return Err(api_error("initialize calendar account", error)),
    };
    binding.account.uid = map.uid().ok_or(BudgetError::Identity)?;
    let stored = decode(&map, &binding)?;
    if stored.usage.daily != 0
        || stored.usage.monthly != 0
        || map.metadata.owner_references != value.metadata.owner_references
    {
        return Err(BudgetError::Corrupt.into());
    }
    let namespaces: Api<Namespace> = Api::all(client.clone());
    namespaces
        .patch(
            &namespace.name_any(),
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {"uid":namespace.uid(),"resourceVersion":namespace.resource_version(),
                    "annotations":{
                        ACCOUNT_ANNOTATION:binding.account.uid,
                        INITIALIZATION_ANNOTATION: null
                    },
                    "labels":{"kars.azure.com/calendar-budget":"v1"}}
            })),
        )
        .await
        .map_err(|e| api_error("seal calendar account identity", e))?;
    Ok(binding)
}

pub async fn prepare_legacy(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), StoreError> {
    let namespace = crate::reconciler::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| BudgetError::Identity)?;
    if namespace.annotations().contains_key(ACCOUNT_ANNOTATION) {
        return Err(StoreError::Api {
            stage: "sealed calendar accounting cannot fall back to volatile usage",
            code: None,
        });
    }
    if namespace
        .annotations()
        .contains_key(INITIALIZATION_ANNOTATION)
    {
        let namespaces: Api<Namespace> = Api::all(client.clone());
        namespaces
            .patch(
                &namespace.name_any(),
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata": {
                        "uid": namespace.uid(), "resourceVersion": namespace.resource_version(),
                        "annotations": {INITIALIZATION_ANNOTATION: null}
                    }
                })),
            )
            .await
            .map_err(|e| {
                api_error(
                    "retire fresh calendar initialization before legacy execution",
                    e,
                )
            })?;
    }
    Ok(())
}

pub struct Authorization {
    lifetime_bound: bool,
    policy: Policy,
}

pub async fn authenticate(
    client: &Client,
    settings: &Settings,
    headers: &HeaderMap,
    identity: &Identity,
    dispatch: bool,
) -> Result<Authorization, StoreError> {
    identity.validate()?;
    let binding = &identity.binding;
    if binding.account.namespace != settings.accounting_namespace {
        return Err(BudgetError::Authorization.into());
    }
    let runtime_namespace = format!("kars-{}", binding.sandbox.name);
    let user = auth::review_router_token(
        client,
        headers,
        &runtime_namespace,
        &identity.pod_name,
        &identity.pod_uid,
    )
    .await?;
    let namespace = Api::<Namespace>::all(client.clone())
        .get(&runtime_namespace)
        .await
        .map_err(|e| api_error("verify calendar namespace", e))?;
    let sandbox = Api::<KarsSandbox>::namespaced(client.clone(), &binding.sandbox.namespace)
        .get(&binding.sandbox.name)
        .await
        .map_err(|e| api_error("verify calendar sandbox", e))?;
    if namespace.uid().as_deref() != Some(&binding.runtime_namespace_uid)
        || namespace.metadata.deletion_timestamp.is_some()
        || namespace.annotations().get(ACCOUNT_ANNOTATION) != Some(&binding.account.uid)
        || namespace
            .labels()
            .get("kars.azure.com/calendar-budget")
            .map(String::as_str)
            != Some("v1")
        || sandbox.uid().as_deref() != Some(&binding.sandbox.uid)
        || sandbox.metadata.deletion_timestamp.is_some()
        || binding.account.name != name(&sandbox)?
        || !crate::reconciler::namespace_ownership::claimed(&namespace, &sandbox)
            .map_err(|_| BudgetError::Identity)?
    {
        return Err(BudgetError::Identity.into());
    }
    let pod = Api::<Pod>::namespaced(client.clone(), &runtime_namespace)
        .get(&identity.pod_name)
        .await
        .map_err(|e| api_error("verify calendar router Pod", e))?;
    if pod.uid().as_deref() != Some(&identity.pod_uid)
        || (dispatch && pod.metadata.deletion_timestamp.is_some())
        || pod
            .spec
            .as_ref()
            .and_then(|spec| spec.service_account_name.as_deref())
            != Some("sandbox")
        || !auth::has_router_only_projection(&pod)
    {
        return Err(BudgetError::Authorization.into());
    }
    auth::verify_workload_owner(client, &pod, &sandbox, &runtime_namespace, dispatch).await?;
    auth::verify_agent_privileges(client, &user, &runtime_namespace, &identity.pod_name).await?;
    let encoded = pod
        .spec
        .as_ref()
        .and_then(|spec| {
            spec.containers
                .iter()
                .find(|container| container.name == "inference-router")
        })
        .and_then(|container| container.env.as_ref())
        .and_then(|env| env.iter().find(|entry| entry.name == BINDING_ENV))
        .and_then(|entry| entry.value.as_deref())
        .ok_or(BudgetError::Identity)?;
    let projected: Binding = serde_json::from_str(encoded).map_err(|_| BudgetError::Corrupt)?;
    if projected != *binding {
        return Err(BudgetError::Identity.into());
    }
    auth::verify_privacy_epoch(client, &runtime_namespace, &binding.privacy_epoch, dispatch)
        .await?;
    let config = settings.operator_config(client).await?;
    let policy = Policy::parse(
        config
            .data
            .as_ref()
            .and_then(|data| data.get(POLICY_KEY))
            .map(String::as_str),
    )?;
    if policy == Policy::RecordOnly {
        super::admission::verify(client, &settings.accounting_namespace).await?;
    }
    Ok(Authorization {
        lifetime_bound: lifetime_projection(&pod, &sandbox, binding),
        policy,
    })
}

fn lifetime_projection(pod: &Pod, sandbox: &KarsSandbox, calendar: &Binding) -> bool {
    let Some(bound) = &sandbox.spec.inference_budget_ref else {
        return false;
    };
    let projected = pod
        .spec
        .as_ref()
        .and_then(|spec| {
            spec.containers
                .iter()
                .find(|c| c.name == "inference-router")
        })
        .and_then(|router| router.env.as_ref())
        .and_then(|env| {
            env.iter()
                .find(|e| e.name == "KARS_INFERENCE_BUDGET_BINDING")
        })
        .filter(|entry| entry.value_from.is_none())
        .and_then(|entry| entry.value.as_deref())
        .and_then(|encoded| {
            serde_json::from_str::<crate::inference_budget_contract::RouterBinding>(encoded).ok()
        });
    projected.is_some_and(|projected| {
        projected.task == *bound
            && projected.sandbox == calendar.sandbox
            && projected.runtime_namespace == format!("kars-{}", sandbox.name_any())
            && projected.runtime_namespace_uid == calendar.runtime_namespace_uid
            && projected.privacy_epoch == calendar.privacy_epoch
    })
}

/// Enroll only a sealed calendar with no acknowledged provider attempt, not a
/// zero current-month counter. Existing identities, counters and receipts remain.
pub(super) async fn fence_unused(
    client: &Client,
    settings: &Settings,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<(), StoreError> {
    if !namespace.annotations().contains_key(ACCOUNT_ANNOTATION) {
        return Err(BudgetError::Authorization.into());
    }
    let binding = prepare(client, settings, sandbox, namespace).await?;
    let maps: Api<ConfigMap> = Api::namespaced(client.clone(), &binding.account.namespace);
    let map = maps
        .get(&binding.account.name)
        .await
        .map_err(|e| api_error("read unused calendar enrollment", e))?;
    let owners = map.owner_references();
    if owners.len() != 1
        || owners[0].api_version != "v1"
        || owners[0].kind != "Namespace"
        || owners[0].name != namespace.name_any()
        || owners[0].uid != binding.runtime_namespace_uid
        || owners[0].controller != Some(true)
    {
        return Err(BudgetError::Identity.into());
    }
    let mut ledger = decode(&map, &binding)?;
    ledger.seal_unused()?;
    if decode(&map, &binding)?.lifetime_only {
        return Ok(());
    }
    let encoded = serde_json::to_string(&ledger).map_err(|_| BudgetError::Corrupt)?;
    let updated = maps
        .patch(
            &binding.account.name,
            &PatchParams::default(),
            &Patch::Merge(json!({
                "metadata": {"uid": map.uid(), "resourceVersion": map.resource_version()},
                "data": {"ledger.json": encoded}
            })),
        )
        .await
        .map_err(|e| api_error("fence unused calendar enrollment", e))?;
    let mut sealed = decode(&updated, &binding)?;
    if !sealed.lifetime_only {
        return Err(BudgetError::Authorization.into());
    }
    sealed.seal_unused()?;
    Ok(())
}

pub async fn read(
    client: &Client,
    binding: &Binding,
    writer: &str,
    authorization: &Authorization,
) -> Result<Observation, StoreError> {
    let map = Api::<ConfigMap>::namespaced(client.clone(), &binding.account.namespace)
        .get(&binding.account.name)
        .await
        .map_err(|e| api_error("read calendar usage", e))?;
    let ledger = decode(&map, binding)?;
    ledger.check_reader_with_policy(writer, authorization.policy)?;
    Ok(ledger.observation(authorization.policy)?)
}

pub async fn commit(
    client: &Client,
    request: &Commit,
    authorization: &Authorization,
) -> Result<Observation, StoreError> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        commit_inner(client, request, authorization),
    )
    .await
    .map_err(|_| StoreError::Contention)?
}

async fn commit_inner(
    client: &Client,
    request: &Commit,
    authorization: &Authorization,
) -> Result<Observation, StoreError> {
    let binding = &request.identity.binding;
    let maps: Api<ConfigMap> = Api::namespaced(client.clone(), &binding.account.namespace);
    for _ in 0..8 {
        let map = maps
            .get(&binding.account.name)
            .await
            .map_err(|e| api_error("read calendar commit version", e))?;
        let mut ledger = decode(&map, binding)?;
        let (day, month) = calendar();
        ledger.commit_with_policy(
            request,
            day,
            month,
            authorization.lifetime_bound,
            authorization.policy,
        )?;
        let encoded = serde_json::to_string(&ledger).map_err(|_| BudgetError::Corrupt)?;
        if encoded.len() > 262_144 {
            return Err(BudgetError::Capacity.into());
        }
        let updated = maps
            .patch(
                &binding.account.name,
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata":{"uid":map.uid(),"resourceVersion":map.resource_version()},
                    "data":{"ledger.json":encoded}
                })),
            )
            .await;
        match updated {
            Ok(updated) => return Ok(decode(&updated, binding)?.observation(authorization.policy)?),
            Err(kube::Error::Api(error)) if error.code == 409 => continue,
            Err(error) => return Err(api_error("persist calendar usage", error)),
        }
    }
    Err(StoreError::Contention)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    struct InitializationState {
        namespace: Namespace,
        account: Option<ConfigMap>,
        deployment: bool,
    }

    async fn initialization_server(
        fresh: bool,
        deployment: bool,
    ) -> (
        MockServer,
        Client,
        KarsSandbox,
        Arc<Mutex<InitializationState>>,
    ) {
        let mut sandbox = KarsSandbox::new("sandbox", Default::default());
        sandbox.metadata.namespace = Some("workspace".into());
        sandbox.metadata.uid = Some("sandbox-uid".into());
        sandbox.metadata.resource_version = Some("1".into());
        let namespace: Namespace = serde_json::from_value(json!({
            "apiVersion":"v1", "kind":"Namespace",
            "metadata": {
                "name":"kars-sandbox", "uid":"namespace-uid", "resourceVersion":"1",
                "annotations": {
                    "kars.azure.com/namespace-claim-version":"v1",
                    "kars.azure.com/sandbox-namespace":"workspace",
                    "kars.azure.com/sandbox-name":"sandbox",
                    "kars.azure.com/sandbox-uid":"sandbox-uid"
                }
            }
        }))
        .unwrap();
        let mut state = InitializationState {
            namespace,
            account: None,
            deployment,
        };
        if fresh {
            state
                .namespace
                .metadata
                .annotations
                .as_mut()
                .unwrap()
                .insert(INITIALIZATION_ANNOTATION.into(), "sandbox-uid".into());
        }
        let state = Arc::new(Mutex::new(state));
        let response = state.clone();
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any()).respond_with(move |request: &Request| {
            let mut state = response.lock().unwrap();
            let not_found = || ResponseTemplate::new(404).set_body_json(json!({
                "apiVersion":"v1", "kind":"Status", "status":"Failure",
                "reason":"NotFound", "message":"not found", "code":404
            }));
            match (request.method.as_str(), request.url.path()) {
                ("GET", "/api/v1/namespaces/kars-sandbox") =>
                    ResponseTemplate::new(200).set_body_json(&state.namespace),
                ("PATCH", "/api/v1/namespaces/kars-sandbox") => {
                    let patch: Value = serde_json::from_slice(&request.body).unwrap();
                    assert_eq!(patch["metadata"]["uid"], "namespace-uid");
                    assert_eq!(patch["metadata"]["resourceVersion"], state.namespace.resource_version().unwrap());
                    for (key, value) in patch["metadata"]["annotations"].as_object().unwrap() {
                        let annotations = state.namespace.metadata.annotations.get_or_insert_default();
                        if value.is_null() {
                            annotations.remove(key);
                        } else {
                            annotations.insert(key.clone(), value.as_str().unwrap().into());
                        }
                    }
                    if let Some(labels) = patch["metadata"].get("labels") {
                        state.namespace.metadata.labels.get_or_insert_default().extend(
                            serde_json::from_value::<std::collections::BTreeMap<String, String>>(labels.clone()).unwrap()
                        );
                    }
                    let version = state.namespace.resource_version().unwrap().parse::<u64>().unwrap() + 1;
                    state.namespace.metadata.resource_version = Some(version.to_string());
                    ResponseTemplate::new(200).set_body_json(&state.namespace)
                }
                ("POST", "/apis/authorization.k8s.io/v1/subjectaccessreviews") =>
                    ResponseTemplate::new(201).set_body_json(json!({
                        "apiVersion":"authorization.k8s.io/v1", "kind":"SubjectAccessReview",
                        "spec":{}, "status":{"allowed":false}
                    })),
                ("GET", path) if path.contains("/karssreregistrations/") => not_found(),
                ("GET", "/apis/apps/v1/namespaces/kars-sandbox/deployments/sandbox") =>
                    if state.deployment {
                        ResponseTemplate::new(200).set_body_json(json!({
                            "apiVersion":"apps/v1", "kind":"Deployment",
                            "metadata":{"name":"sandbox"}
                        }))
                    } else { not_found() },
                ("POST", "/api/v1/namespaces/accounting/configmaps") => {
                    assert!(state.account.is_none(), "must not replace an account");
                    let mut map: ConfigMap = serde_json::from_slice(&request.body).unwrap();
                    map.metadata.uid = Some("account-uid".into());
                    map.metadata.resource_version = Some("1".into());
                    state.account = Some(map.clone());
                    ResponseTemplate::new(201).set_body_json(map)
                }
                ("PATCH", path) if path.starts_with("/api/v1/namespaces/accounting/configmaps/") => {
                    let patch: Value = serde_json::from_slice(&request.body).unwrap();
                    let map = state.account.as_mut().unwrap();
                    assert_eq!(patch["metadata"]["uid"], map.uid().unwrap());
                    if patch["metadata"]["resourceVersion"] != map.resource_version().unwrap() {
                        return ResponseTemplate::new(409).set_body_json(json!({
                            "apiVersion":"v1", "kind":"Status", "status":"Failure",
                            "reason":"Conflict", "message":"changed", "code":409
                        }));
                    }
                    map.data.as_mut().unwrap().insert("ledger.json".into(), patch["data"]["ledger.json"].as_str().unwrap().into());
                    map.metadata.resource_version = Some((map.resource_version().unwrap().parse::<u64>().unwrap() + 1).to_string());
                    ResponseTemplate::new(200).set_body_json(map)
                }
                ("GET", path) if path.starts_with("/api/v1/namespaces/accounting/configmaps/") =>
                    match &state.account {
                        Some(map) => ResponseTemplate::new(200).set_body_json(map),
                        None => not_found(),
                    },
                _ => panic!("unexpected calendar initialization request: {} {}", request.method, request.url),
            }
        }).mount(&server).await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        (server, client, sandbox, state)
    }

    fn settings() -> Settings {
        Settings {
            accounting_namespace: "accounting".into(),
            catalog_name: "contracts".into(),
            address: "0.0.0.0:9447".into(),
            tls_secret: "tls".into(),
            router_image_digest: format!("sha256:{}", "a".repeat(64)),
            first_install_router_digest: None,
        }
    }

    #[tokio::test]
    async fn calendar_missing_legacy_deployment_does_not_authorize_zero_initialization() {
        for (fresh, deployment) in [(false, false), (false, true), (true, true)] {
            let (server, client, sandbox, state) = initialization_server(fresh, deployment).await;
            let namespace = state.lock().unwrap().namespace.clone();
            let error = prepare(&client, &settings(), &sandbox, &namespace)
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("legacy calendar usage"),
                "{error}"
            );
            assert!(state.lock().unwrap().account.is_none());
            assert!(
                !server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .any(|r| r.url.path().contains("/configmaps"))
            );
        }
    }

    #[tokio::test]
    async fn calendar_fresh_initialization_consumes_provenance_and_reuses_sealed_balance() {
        let (_server, client, sandbox, state) = initialization_server(true, false).await;
        let namespace = state.lock().unwrap().namespace.clone();
        let binding = prepare(&client, &settings(), &sandbox, &namespace)
            .await
            .unwrap();
        let (namespace, original) = {
            let mut state = state.lock().unwrap();
            assert!(
                !state
                    .namespace
                    .annotations()
                    .contains_key(INITIALIZATION_ANNOTATION)
            );
            let map = state.account.as_mut().unwrap();
            let mut ledger = decode(map, &binding).unwrap();
            ledger.usage.daily = 39;
            ledger.usage.monthly = 39;
            map.data.as_mut().unwrap().insert(
                "ledger.json".into(),
                serde_json::to_string(&ledger).unwrap(),
            );
            (state.namespace.clone(), state.account.clone().unwrap())
        };
        assert_eq!(
            prepare(&client, &settings(), &sandbox, &namespace)
                .await
                .unwrap(),
            binding
        );
        assert_eq!(state.lock().unwrap().account.as_ref().unwrap(), &original);
        assert!(
            prepare_legacy(&client, &sandbox, &namespace)
                .await
                .unwrap_err()
                .to_string()
                .contains("cannot fall back")
        );
    }

    #[tokio::test]
    async fn calendar_legacy_execution_retires_fresh_initialization_even_after_deployment_removal()
    {
        let (_server, client, sandbox, state) = initialization_server(true, false).await;
        let namespace = state.lock().unwrap().namespace.clone();
        prepare_legacy(&client, &sandbox, &namespace).await.unwrap();
        let namespace = state.lock().unwrap().namespace.clone();
        assert!(
            !namespace
                .annotations()
                .contains_key(INITIALIZATION_ANNOTATION)
        );
        prepare_legacy(&client, &sandbox, &namespace).await.unwrap();
        assert!(
            prepare(&client, &settings(), &sandbox, &namespace)
                .await
                .unwrap_err()
                .to_string()
                .contains("legacy calendar usage")
        );
        assert!(state.lock().unwrap().account.is_none());
    }

    fn identity() -> Identity {
        let mut sandbox = KarsSandbox::new("sandbox", Default::default());
        sandbox.metadata.uid = Some("sandbox-uid".into());
        Identity {
            binding: Binding {
                sandbox: ResourceIdentity {
                    namespace: "workspace".into(),
                    name: "sandbox".into(),
                    uid: "sandbox-uid".into(),
                },
                runtime_namespace_uid: "namespace-uid".into(),
                account: ResourceIdentity {
                    namespace: "accounting".into(),
                    name: name(&sandbox).unwrap(),
                    uid: "account-uid".into(),
                },
                privacy_epoch: None,
            },
            pod_name: "pod".into(),
            pod_uid: "pod-uid".into(),
        }
    }

    #[test]
    fn calendar_orphan_finalization_requires_its_atomic_operator_audit_on_every_read() {
        let identity = identity();
        let before = json!({"podUid":"retired-pod", "sequence":77, "tokens":0, "month":1, "event":"begin", "pending":1});
        let mut after = before.clone();
        after["orphanFinalization"] = json!({
            "version":"calendar-account-bound/v1", "accountUid":"lifetime-uid",
            "accountResourceVersion":"10", "calendarResourceVersion":"20",
            "lifetimeLedgerSha256":"a".repeat(64), "maximumTokens":17, "day":1, "month":1,
        });
        let usage_before = json!({"day":1,"month":1,"daily":3,"monthly":5});
        let usage_after = json!({"day":1,"month":1,"daily":20,"monthly":22});
        let ledger = json!({"sandbox":identity.binding.sandbox, "runtimeNamespaceUid":"namespace-uid",
            "usage":usage_after, "writers":{"writer":after}});
        let mut map: ConfigMap = serde_json::from_value(json!({
            "metadata":{"uid":"account-uid","resourceVersion":"21"},
            "data":{"ledger.json":ledger.to_string()}
        }))
        .unwrap();
        assert!(decode(&map, &identity.binding).is_err());
        let audit = json!({"writer:77":{
            "version":"calendar-account-bound/v1", "calendarUid":"account-uid", "beforeResourceVersion":"20",
            "account":{"uid":"lifetime-uid"}, "accountResourceVersion":"10", "lifetimeLedgerSha256":"a".repeat(64),
            "actualUsage":"unknown", "lifetimeChargeChanged":false, "newFunding":false,
            "writerBefore":before, "writerAfter":after, "usageBefore":usage_before, "usageAfter":usage_after,
        }});
        map.annotations_mut().insert(
            "kars.azure.com/calendar-reconciliations".into(),
            audit.to_string(),
        );
        let decoded = decode(&map, &identity.binding).unwrap();
        assert!(decoded.check_reader("replacement").is_ok());
        assert!(decoded.check_reader("writer").is_err());
        assert_eq!(decoded.usage.monthly, 22);
        let mut corrupt = audit;
        corrupt["writer:77"]["usageAfter"]["monthly"] = json!(21);
        map.annotations_mut().insert(
            "kars.azure.com/calendar-reconciliations".into(),
            corrupt.to_string(),
        );
        assert!(decode(&map, &identity.binding).is_err());
    }

    #[tokio::test]
    async fn calendar_cas_and_lost_acknowledgement_preserve_usage_once() {
        let identity = identity();
        assert_eq!(identity.binding.account.name.len(), 73);
        identity.validate().unwrap();
        let ledger = Ledger::new(
            identity.binding.sandbox.clone(),
            "namespace-uid".into(),
            0,
            0,
        );
        let map: ConfigMap = serde_json::from_value(json!({
                    "apiVersion":"v1","kind":"ConfigMap",
                    "metadata":{"name":identity.binding.account.name,"namespace":"accounting","uid":"account-uid","resourceVersion":"1"},
                    "data":{"ledger.json":serde_json::to_string(&ledger).unwrap()}
                })).unwrap();
        let state = Arc::new(Mutex::new((map, false)));
        let responder = state.clone();
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::path(format!("/api/v1/namespaces/accounting/configmaps/{}", identity.binding.account.name)))
                    .respond_with(move |request: &Request| {
                        let mut state = responder.lock().unwrap();
                        if request.method.as_str() == "PATCH" {
                            if !state.1 {
                                state.1 = true;
                                state.0.metadata.resource_version = Some("2".into());
                                return ResponseTemplate::new(409).set_body_json(json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Conflict","message":"test contention","code":409}));
                            }
                            let patch: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                            assert_eq!(patch["metadata"]["uid"], "account-uid");
                            assert_eq!(patch["metadata"]["resourceVersion"], state.0.resource_version().unwrap());
                            state.0.data.as_mut().unwrap().insert("ledger.json".into(), patch["data"]["ledger.json"].as_str().unwrap().into());
                            state.0.metadata.resource_version = Some((state.0.resource_version().unwrap().parse::<u64>().unwrap() + 1).to_string());
                        }
                        ResponseTemplate::new(200).set_body_json(&state.0)
                    }).mount(&server).await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let request = Commit {
            identity: identity.clone(),
            writer: "writer".into(),
            sequence: 2,
            tokens: 39,
            event: crate::inference_budget_contract::calendar::Event::Finish,
        };
        let mut begin = request.clone();
        begin.sequence = 1;
        begin.tokens = 0;
        begin.event = crate::inference_budget_contract::calendar::Event::Begin;
        let authorization = Authorization {
            lifetime_bound: false,
            policy: Policy::Enforce,
        };
        commit(&client, &begin, &authorization).await.unwrap();
        for _ in 0..2 {
            assert_eq!(
                commit(&client, &request, &authorization)
                    .await
                    .unwrap()
                    .usage
                    .monthly,
                39
            );
        }
        assert_eq!(
            read(&client, &identity.binding, "writer", &authorization)
                .await
                .unwrap()
                .usage
                .monthly,
            39
        );
        let mut orphan = begin.clone();
        orphan.writer = "orphan-writer".into();
        orphan.identity.pod_uid = "orphan-pod".into();
        commit(&client, &orphan, &authorization).await.unwrap();
        let retained = decode(&state.lock().unwrap().0, &identity.binding).unwrap();
        let retained = serde_json::to_value(retained).unwrap()["writers"]["orphan-writer"].clone();
        assert!(
            read(&client, &identity.binding, "writer", &authorization)
                .await
                .is_err()
        );
        let record_only = Authorization {
            lifetime_bound: false,
            policy: Policy::RecordOnly,
        };
        let observation = read(&client, &identity.binding, "writer", &record_only)
            .await
            .unwrap();
        assert_eq!(observation.pending_requests, Some(1));
        assert_eq!(observation.usage.monthly, 39);
        begin.sequence = 3;
        commit(&client, &begin, &record_only).await.unwrap();
        let mut finish = request.clone();
        finish.sequence = 4;
        assert_eq!(
            commit(&client, &finish, &record_only)
                .await
                .unwrap()
                .usage
                .monthly,
            78
        );
        let persisted = decode(&state.lock().unwrap().0, &identity.binding).unwrap();
        assert_eq!(
            serde_json::to_value(persisted).unwrap()["writers"]["orphan-writer"],
            retained
        );
        assert!(
            read(&client, &identity.binding, "writer", &authorization)
                .await
                .is_err()
        );
        let mut replaced = identity.binding.clone();
        replaced.account.uid = "replaced".into();
        assert!(
            read(&client, &replaced, "writer", &authorization)
                .await
                .is_err()
        );
        state
            .lock()
            .unwrap()
            .0
            .data
            .as_mut()
            .unwrap()
            .insert("ledger.json".into(), "{}".into());
        assert!(
            read(&client, &identity.binding, "writer", &authorization)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn unused_enrollment_preserves_calendar_and_denies_legacy_commits() {
        let (_server, client, sandbox, state) = initialization_server(true, false).await;
        let namespace = state.lock().unwrap().namespace.clone();
        let binding = prepare(&client, &settings(), &sandbox, &namespace)
            .await
            .unwrap();
        let namespace = state.lock().unwrap().namespace.clone();
        state.lock().unwrap().deployment = true;
        fence_unused(&client, &settings(), &sandbox, &namespace)
            .await
            .unwrap();
        fence_unused(&client, &settings(), &sandbox, &namespace)
            .await
            .unwrap();
        assert_eq!(state.lock().unwrap().namespace, namespace);
        assert_eq!(
            prepare(&client, &settings(), &sandbox, &namespace)
                .await
                .unwrap(),
            binding
        );
        let request = Commit {
            identity: identity(),
            writer: "writer".into(),
            sequence: 1,
            tokens: 0,
            event: crate::inference_budget_contract::calendar::Event::Begin,
        };
        assert!(
            commit(
                &client,
                &request,
                &Authorization {
                    lifetime_bound: false,
                    policy: Policy::Enforce,
                }
            )
            .await
            .is_err()
        );
        commit(
            &client,
            &request,
            &Authorization {
                lifetime_bound: true,
                policy: Policy::Enforce,
            },
        )
        .await
        .unwrap();
        assert!(
            fence_unused(&client, &settings(), &sandbox, &namespace)
                .await
                .is_err()
        );
        let ledger = decode(state.lock().unwrap().account.as_ref().unwrap(), &binding).unwrap();
        assert!(ledger.lifetime_only);
        assert_eq!(ledger.usage.monthly, 0);
    }

    #[tokio::test]
    async fn unused_enrollment_rejects_missing_replaced_used_and_foreign_history() {
        for mutation in [
            "missing",
            "replacement",
            "owner",
            "usage",
            "writer",
            "corrupt",
        ] {
            let (_server, client, sandbox, state) = initialization_server(true, false).await;
            let namespace = state.lock().unwrap().namespace.clone();
            let binding = prepare(&client, &settings(), &sandbox, &namespace)
                .await
                .unwrap();
            let namespace = state.lock().unwrap().namespace.clone();
            {
                let mut state = state.lock().unwrap();
                let map = state.account.as_mut().unwrap();
                match mutation {
                    "missing" => state.account = None,
                    "replacement" => map.metadata.uid = Some("replacement".into()),
                    "owner" => {
                        map.metadata.owner_references.as_mut().unwrap()[0].uid = "foreign".into()
                    }
                    "corrupt" => {
                        map.data
                            .as_mut()
                            .unwrap()
                            .insert("ledger.json".into(), "{}".into());
                    }
                    _ => {
                        let mut ledger = decode(map, &binding).unwrap();
                        if mutation == "usage" {
                            ledger.usage.monthly = 1;
                        } else {
                            ledger.commit(&Commit { identity: identity(), writer: "writer".into(), sequence: 1,
                            tokens: 0, event: crate::inference_budget_contract::calendar::Event::Begin },
                            ledger.usage.day, ledger.usage.month).unwrap();
                        }
                        map.data.as_mut().unwrap().insert(
                            "ledger.json".into(),
                            serde_json::to_string(&ledger).unwrap(),
                        );
                    }
                }
            }
            let before = state.lock().unwrap().account.clone();
            assert!(
                fence_unused(&client, &settings(), &sandbox, &namespace)
                    .await
                    .is_err(),
                "{mutation}"
            );
            assert_eq!(state.lock().unwrap().account, before, "{mutation}");
        }
    }

    #[tokio::test]
    async fn enrollment_and_provider_begin_cas_races_fail_closed_in_both_orders() {
        for enrollment_wins in [false, true] {
            let (server, client, sandbox, state) = initialization_server(true, false).await;
            let namespace = state.lock().unwrap().namespace.clone();
            let binding = prepare(&client, &settings(), &sandbox, &namespace)
                .await
                .unwrap();
            let namespace = state.lock().unwrap().namespace.clone();
            let begin = Commit {
                identity: identity(),
                writer: "writer".into(),
                sequence: 1,
                tokens: 0,
                event: crate::inference_budget_contract::calendar::Event::Begin,
            };
            let responder = state.clone();
            let racing_begin = begin.clone();
            Mock::given(wiremock::matchers::method("PATCH"))
                .and(wiremock::matchers::path(format!("/api/v1/namespaces/accounting/configmaps/{}", binding.account.name)))
                .respond_with(move |_: &Request| {
                    let mut state = responder.lock().unwrap();
                    let map = state.account.as_mut().unwrap();
                    let mut ledger = decode(map, &racing_begin.identity.binding).unwrap();
                    if enrollment_wins { ledger.seal_unused().unwrap(); }
                    else { ledger.commit(&racing_begin, ledger.usage.day, ledger.usage.month).unwrap(); }
                    map.data.as_mut().unwrap().insert("ledger.json".into(), serde_json::to_string(&ledger).unwrap());
                    map.metadata.resource_version = Some("2".into());
                    ResponseTemplate::new(409).set_body_json(json!({
                        "apiVersion":"v1", "kind":"Status", "status":"Failure", "reason":"Conflict", "message":"changed", "code":409
                    }))
                }).with_priority(1).up_to_n_times(1).expect(1).mount(&server).await;
            if enrollment_wins {
                assert!(
                    commit(
                        &client,
                        &begin,
                        &Authorization {
                            lifetime_bound: false,
                            policy: Policy::Enforce,
                        }
                    )
                    .await
                    .is_err()
                );
            } else {
                assert!(
                    fence_unused(&client, &settings(), &sandbox, &namespace)
                        .await
                        .is_err()
                );
                assert!(
                    fence_unused(&client, &settings(), &sandbox, &namespace)
                        .await
                        .is_err()
                );
            }
            let ledger = decode(state.lock().unwrap().account.as_ref().unwrap(), &binding).unwrap();
            assert_eq!(ledger.lifetime_only, enrollment_wins);
            server.verify().await;
        }
    }

    #[test]
    fn lifetime_permission_requires_the_exact_live_router_projection() {
        let calendar = identity().binding;
        let mut sandbox = KarsSandbox::new("sandbox", Default::default());
        sandbox.metadata.namespace = Some("workspace".into());
        sandbox.metadata.uid = Some("sandbox-uid".into());
        let bound = serde_json::from_value(json!({
            "scope":"GovernedInference", "account":{"namespace":"accounting","name":"account","uid":"lifetime-uid"},
            "root":{"kind":"KarsSandbox","resource":calendar.sandbox,"workspaceUid":"workspace-uid","clusterUid":"cluster-uid"},
            "taskUid":"sandbox-uid","rootTaskUid":"sandbox-uid","parentTaskUid":null,"authorizationDigest":"digest"
        })).unwrap();
        sandbox.spec.inference_budget_ref = Some(bound);
        let projection = json!({"task":sandbox.spec.inference_budget_ref,"sandbox":calendar.sandbox,
            "runtimeNamespace":"kars-sandbox","runtimeNamespaceUid":"namespace-uid","privacyEpoch":null});
        for mutation in [
            "none",
            "missing",
            "account",
            "sandbox",
            "namespace",
            "epoch",
            "agent",
            "indirect",
            "unbound",
        ] {
            let mut projected = projection.clone();
            let mut source = sandbox.clone();
            match mutation {
                "account" => projected["task"]["account"]["uid"] = json!("replacement"),
                "sandbox" => projected["sandbox"]["uid"] = json!("replacement"),
                "namespace" => projected["runtimeNamespaceUid"] = json!("replacement"),
                "epoch" => projected["privacyEpoch"] = json!("replacement"),
                "unbound" => source.spec.inference_budget_ref = None,
                _ => {}
            }
            let mut pod: Pod =
                serde_json::from_value(json!({"spec":{"containers":[{"name":"inference-router",
                "env":[{"name":"KARS_INFERENCE_BUDGET_BINDING","value":projected.to_string()}]}]}}))
                .unwrap();
            let router = &mut pod.spec.as_mut().unwrap().containers[0];
            if mutation == "missing" {
                router.env = None;
            }
            if mutation == "agent" {
                router.name = "openclaw".into();
            }
            if mutation == "indirect" {
                router.env.as_mut().unwrap()[0].value_from = Some(Default::default());
            }
            assert_eq!(
                lifetime_projection(&pod, &source, &calendar),
                mutation == "none",
                "{mutation}"
            );
        }
    }

    #[tokio::test]
    async fn calendar_requests_without_private_token_never_access_accounts() {
        let server = MockServer::start().await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let settings = Settings {
            accounting_namespace: "accounting".into(),
            catalog_name: "contracts".into(),
            address: "0.0.0.0:9447".into(),
            tls_secret: "tls".into(),
            router_image_digest: format!("sha256:{}", "a".repeat(64)),
            first_install_router_digest: None,
        };
        assert!(
            authenticate(&client, &settings, &HeaderMap::new(), &identity(), true)
                .await
                .is_err()
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
