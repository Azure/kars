// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Explicit operator enrollment of a real built-in Sandbox UID. Reuses the
//! account store and router protocol; never imports calendar totals as lifetime
//! history or creates a Task to stand in for a built-in actor.

use super::{
    account::KarsBudgetAccountSpec,
    config::Settings,
    store::{Store, StoreError},
};
use crate::{
    crd::{KarsSandbox, RuntimeKind},
    inference_budget_contract::{
        AccountReference, BudgetError, BudgetScope, BuiltinFunding, ExecutionIdentity, RootKind,
        TaskAuthority, TaskBudgetBinding, ledger::Ledger,
    },
};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Pod},
};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
};
use serde_json::json;

const CONFIG_KEY: &str = "built-in-funding.json";

fn api_error(error: kube::Error) -> StoreError {
    StoreError::Api {
        stage: "verify or enroll built-in lifetime funding",
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

fn parse_config(encoded: &str) -> Result<Vec<BuiltinFunding>, StoreError> {
    if encoded.len() > 32_768 {
        return Err(BudgetError::Capacity.into());
    }
    let grants: Vec<BuiltinFunding> =
        serde_json::from_str(encoded).map_err(|_| BudgetError::Authorization)?;
    let mut actors = std::collections::BTreeSet::new();
    for grant in &grants {
        // Expired entries remain parseable so they cannot disable unrelated actors.
        grant.validate(i64::MIN)?;
        if !actors.insert(grant.root.resource.name.clone()) {
            return Err(BudgetError::Authorization.into());
        }
    }
    Ok(grants)
}

async fn configured(
    client: &Client,
    sandbox: &KarsSandbox,
) -> Result<Option<BuiltinFunding>, StoreError> {
    if !matches!(sandbox.name_any().as_str(), "sre" | "bridge-orchestrator") {
        return Ok(None);
    }
    let Some(settings) = Settings::from_env()? else {
        if sandbox
            .annotations()
            .contains_key(super::first_install::ANNOTATION)
        {
            return Err(BudgetError::Authorization.into());
        }
        return Ok(None);
    };
    configured_from_store(client, &settings, sandbox).await
}

async fn configured_from_store(
    client: &Client,
    settings: &Settings,
    sandbox: &KarsSandbox,
) -> Result<Option<BuiltinFunding>, StoreError> {
    let config = settings.operator_config(client).await?;
    let encoded = config.data.as_ref().and_then(|data| data.get(CONFIG_KEY));
    let grants = parse_config(encoded.map(String::as_str).unwrap_or("[]"))?;
    if sandbox
        .annotations()
        .contains_key(super::first_install::ANNOTATION)
        || (sandbox.name_any() == "sre"
            && config
                .data
                .as_ref()
                .is_some_and(|data| data.contains_key("first-install-sre-intent")))
    {
        if grants.iter().any(|grant| grant.root.resource.name == "sre") {
            return Err(BudgetError::Authorization.into());
        }
        return super::first_install::funding(client, settings, sandbox, &config)
            .await
            .map(Some);
    }
    // Select by name only to fail closed on replacement, never to adopt its UID.
    Ok(grants
        .into_iter()
        .find(|grant| grant.root.resource.name == sandbox.name_any()))
}

pub(super) async fn selected(client: &Client, sandbox: &KarsSandbox) -> Result<bool, StoreError> {
    if sandbox
        .spec
        .inference_budget_ref
        .as_ref()
        .is_some_and(|bound| bound.root.kind == RootKind::KarsSandbox)
    {
        return Ok(true);
    }
    let grant = configured(client, sandbox).await?;
    if let Some(grant) = &grant {
        verify_source(grant, sandbox, chrono::Utc::now().timestamp())?;
    }
    Ok(grant.is_some())
}

fn verify_source(
    grant: &BuiltinFunding,
    sandbox: &KarsSandbox,
    now: i64,
) -> Result<(), StoreError> {
    grant.validate(now)?;
    if sandbox.uid().as_deref() != Some(&grant.root.resource.uid)
        || sandbox.namespace().as_deref() != Some(&grant.root.resource.namespace)
        || sandbox.name_any() != grant.root.resource.name
        || sandbox
            .resource_version()
            .as_deref()
            .is_none_or(str::is_empty)
        || sandbox.metadata.deletion_timestamp.is_some()
        || !sandbox.owner_references().is_empty()
        || (sandbox.name_any() == "sre" && sandbox.spec.runtime.kind != RuntimeKind::Hermes)
    {
        return Err(BudgetError::Identity.into());
    }
    Ok(())
}

pub(super) fn authority(grant: &BuiltinFunding) -> Result<TaskAuthority, StoreError> {
    let mut effective = serde_json::to_value(grant).map_err(|_| BudgetError::Corrupt)?;
    effective.sort_all_objects();
    let bytes = serde_json::to_vec(&effective).map_err(|_| BudgetError::Corrupt)?;
    Ok(TaskAuthority {
        task: grant.root.resource.clone(),
        parent_uid: None,
        root_task_uid: grant.root.resource.uid.clone(),
        authorization_digest: format!("sha256:{}", crate::providers::signing::sha256_hex(&bytes)),
        effective_authorization: effective,
        limits: grant.limits.normalized(),
    })
}

pub(super) fn binding(
    grant: &BuiltinFunding,
    account: AccountReference,
) -> Result<TaskBudgetBinding, StoreError> {
    Ok(TaskBudgetBinding {
        scope: BudgetScope::GovernedInference,
        account,
        root: grant.root.clone(),
        task_uid: grant.root.resource.uid.clone(),
        parent_task_uid: None,
        root_task_uid: grant.root.resource.uid.clone(),
        authorization_digest: authority(grant)?.authorization_digest,
    })
}

fn verify_fresh_history(
    sandbox: &KarsSandbox,
    namespace: &Namespace,
    deployment_exists: bool,
) -> Result<(), StoreError> {
    if sandbox.uid().is_none()
        || namespace
            .annotations()
            .get(super::calendar::INITIALIZATION_ANNOTATION)
            != sandbox.metadata.uid.as_ref()
        || namespace
            .annotations()
            .contains_key(crate::inference_budget_contract::calendar::ACCOUNT_ANNOTATION)
        || deployment_exists
    {
        return Err(StoreError::Api {
            stage: "built-in lifetime migration requires trustworthy complete usage and a verified drain; calendar history is insufficient",
            code: None,
        });
    }
    Ok(())
}

pub(super) fn verify_grant(
    grant: &BuiltinFunding,
    ledger: &Ledger,
    sandbox: &KarsSandbox,
    now: i64,
) -> Result<(), StoreError> {
    verify_source(grant, sandbox, i64::MIN)?;
    ledger.validate_funding_expiry(grant, now)?;
    let node = ledger
        .nodes
        .get(&grant.root.resource.uid)
        .ok_or(BudgetError::Authorization)?;
    if grant.root != ledger.root
        || grant.limits.normalized() != ledger.limits
        || authority(grant)? != node.authority
        || !node.active
    {
        return Err(BudgetError::Authorization.into());
    }
    Ok(())
}

pub(super) async fn verify_registration(
    client: &Client,
    sandbox: &KarsSandbox,
    runtime_namespace_uid: &str,
    epoch: &Option<String>,
) -> Result<(), StoreError> {
    if sandbox.name_any() != "sre" {
        return Ok(());
    }
    let registration = Api::<crate::sre_registration::KarsSRERegistration>::all(client.clone())
        .get(crate::sre_registration::NAME)
        .await
        .map_err(api_error)?;
    if !registration.spec.enabled
        || epoch.as_deref() != Some(registration.epoch().as_str())
        || sandbox.uid().as_deref() != Some(registration.spec.sandbox.uid.as_str())
        || sandbox.namespace().as_deref() != Some(registration.spec.sandbox.namespace.as_str())
        || sandbox.name_any() != registration.spec.sandbox.name
        || runtime_namespace_uid != registration.spec.runtime_namespace.uid
    {
        return Err(BudgetError::Authorization.into());
    }
    Ok(())
}

/// Returns None only for an unselected, unbound actor. A removed or changed
/// funding declaration must never downgrade an enrolled actor to calendar-only.
pub async fn prepare(
    client: &Client,
    sandbox: &KarsSandbox,
    namespace: &Namespace,
) -> Result<Option<TaskBudgetBinding>, StoreError> {
    let grant = configured(client, sandbox).await?;
    let Some(grant) = grant else {
        return if sandbox.spec.inference_budget_ref.is_some() {
            Err(BudgetError::Authorization.into())
        } else {
            Ok(None)
        };
    };
    let settings = Settings::from_env()?.ok_or(BudgetError::Contract)?;
    if let Some(bound) = &sandbox.spec.inference_budget_ref {
        let store = Store::new(client.clone(), &settings.accounting_namespace);
        let account = store.read_anchor(&grant.root, &bound.account.uid).await?;
        if account
            .annotations()
            .get(super::account::BOOTSTRAP)
            .map(String::as_str)
            == Some("pending")
            || Store::ledger(&account).is_ok_and(|ledger| ledger.nodes.is_empty())
        {
            verify_source(&grant, sandbox, chrono::Utc::now().timestamp())?;
            Store::bootstrap_ledger(&account)?;
        } else {
            verify_grant(
                &grant,
                Store::ledger(&account)?,
                sandbox,
                chrono::Utc::now().timestamp(),
            )?;
        }
    } else {
        verify_source(&grant, sandbox, chrono::Utc::now().timestamp())?;
    }
    settings
        .catalog(client, chrono::Utc::now().timestamp())
        .await?;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let workspace = namespaces
        .get(&grant.root.resource.namespace)
        .await
        .map_err(api_error)?;
    let cluster = namespaces.get("kube-system").await.map_err(api_error)?;
    if workspace.uid().as_deref() != Some(&grant.root.workspace_uid)
        || cluster.uid().as_deref() != Some(&grant.root.cluster_uid)
        || workspace.metadata.deletion_timestamp.is_some()
    {
        return Err(BudgetError::Identity.into());
    }
    let namespace = crate::reconciler::namespace_ownership::recheck(client, sandbox, namespace)
        .await
        .map_err(|_| BudgetError::Identity)?;
    let epoch = crate::sre_authority::privacy_epoch(client, &namespace.name_any())
        .await
        .map_err(|_| BudgetError::Authorization)?;
    verify_registration(
        client,
        sandbox,
        &namespace.uid().ok_or(BudgetError::Identity)?,
        &epoch,
    )
    .await?;
    let store = Store::new(client.clone(), &settings.accounting_namespace);
    let bound = if let Some(bound) = &sandbox.spec.inference_budget_ref {
        if *bound != binding(&grant, bound.account.clone())?
            || bound.account.namespace != settings.accounting_namespace
            || bound.account.name != super::account::name_for_root(&grant.root)
        {
            return Err(BudgetError::Identity.into());
        }
        bound.clone()
    } else {
        if namespace
            .annotations()
            .contains_key(crate::inference_budget_contract::calendar::ACCOUNT_ANNOTATION)
        {
            super::calendar::fence_unused(client, &settings, sandbox, &namespace).await?;
            super::pod::fence_namespace(client, sandbox, &namespace).await?;
        } else {
            // Missing history is never a zero opening balance.
            verify_fresh_history(sandbox, &namespace, false)?;
            super::pod::fence_namespace(client, sandbox, &namespace).await?;
            let namespace =
                crate::reconciler::namespace_ownership::recheck(client, sandbox, &namespace)
                    .await
                    .map_err(|_| BudgetError::Identity)?;
            let deployment_exists =
                Api::<Deployment>::namespaced(client.clone(), &namespace.name_any())
                    .get_opt(&sandbox.name_any())
                    .await
                    .map_err(api_error)?
                    .is_some();
            verify_fresh_history(sandbox, &namespace, deployment_exists)?;
        }
        let signer = crate::providers::signing::load_existing(client)
            .await
            .map_err(|_| BudgetError::Authorization)?;
        let anchor = store
            .create_anchor(
                KarsBudgetAccountSpec {
                    scope: BudgetScope::GovernedInference,
                    root: grant.root.clone(),
                    limits: grant.limits.normalized(),
                },
                &signer,
            )
            .await?;
        // Only an interrupted, still-empty bootstrap can be resumed without a pin.
        if anchor
            .annotations()
            .get(super::account::BOOTSTRAP)
            .map(String::as_str)
            != Some("pending")
        {
            return Err(StoreError::Missing);
        }
        Store::bootstrap_ledger(&anchor)?;
        let bound = binding(
            &grant,
            AccountReference {
                namespace: settings.accounting_namespace.clone(),
                name: anchor.name_any(),
                uid: anchor.uid().ok_or(BudgetError::Identity)?,
            },
        )?;
        let api: Api<KarsSandbox> = Api::namespaced(client.clone(), &grant.root.resource.namespace);
        let updated = api.patch(&sandbox.name_any(), &PatchParams::default(), &Patch::Merge(json!({
            "metadata": {"uid": sandbox.uid(), "resourceVersion": sandbox.resource_version()},
            "spec": {"inferenceBudgetRef": bound}
        }))).await.map_err(api_error)?;
        if updated.spec.inference_budget_ref.as_ref() != Some(&bound) {
            return Err(StoreError::Missing);
        }
        bound
    };
    store.initialize(&grant.root, &bound.account.uid).await?;
    let authority = authority(&grant)?;
    store
        .transact(&grant.root, &bound.account.uid, |ledger| {
            ledger.register_task(authority.clone())
        })
        .await?;
    if grant.unbounded_tokens {
        let account = store.read(&grant.root, &bound.account.uid).await?;
        if Store::ledger(&account)?.unbounded_tokens.is_none() {
            return Err(BudgetError::Authorization.into());
        }
    }
    Ok(Some(bound))
}

pub(super) fn verify_binding(
    ledger: &Ledger,
    sandbox: &KarsSandbox,
    identity: &ExecutionIdentity,
    dispatch: bool,
) -> Result<(), StoreError> {
    let bound = sandbox
        .spec
        .inference_budget_ref
        .as_ref()
        .ok_or(BudgetError::Authorization)?;
    let node = ledger
        .nodes
        .get(&identity.task_uid)
        .ok_or(BudgetError::Authorization)?;
    if ledger.root.kind != RootKind::KarsSandbox
        || ledger.root.resource != identity.sandbox
        || identity.task_uid != identity.sandbox.uid
        || bound.root != ledger.root
        || bound.account.uid != ledger.account_uid
        || bound.account.name != super::account::name_for_root(&ledger.root)
        || bound.task_uid != identity.task_uid
        || bound.root_task_uid != identity.task_uid
        || bound.parent_task_uid.is_some()
        || bound.authorization_digest != identity.authorization_digest
        || node.authority.authorization_digest != identity.authorization_digest
    {
        return Err(BudgetError::Identity.into());
    }
    if !dispatch
        && ledger
            .sessions
            .get(&identity.pod_uid)
            .is_none_or(|session| session.identity != *identity)
    {
        return Err(BudgetError::Identity.into());
    }
    Ok(())
}

/// An expired Bridge grant is retained authority, not permission to bootstrap a
/// new identity. Re-read its original catalog entry and exact live source binding
/// before each renewal CAS and subsequent authority read.
pub(super) fn verify_bridge_renewal(
    grant: &crate::inference_budget_contract::ledger::TokenTopUp,
    catalog: &k8s_openapi::api::core::v1::ConfigMap,
    source: &KarsSandbox,
    accounting_namespace: &str,
) -> Result<(), StoreError> {
    grant.validate(i64::MIN)?;
    let renewal = grant
        .funding_renewal
        .as_ref()
        .ok_or(BudgetError::Authorization)?;
    if renewal.bridge_reactivation.is_none() {
        return Err(BudgetError::Authorization.into());
    }
    let original = verify_bridge_funding_source(
        &grant.config_uid, &grant.account_uid, &grant.root, grant.original_limits,
        catalog, source, accounting_namespace,
    )?;
    if original.expires_at != renewal.original_expires_at
        || grant.nodes != std::collections::BTreeMap::from([(
            original.root.resource.uid.clone(), authority(&original)?.authorization_digest,
        )])
    {
        return Err(BudgetError::Authorization.into());
    }
    Ok(())
}

pub(super) fn verify_bridge_funding_source(
    config_uid: &str,
    account_uid: &str,
    root: &crate::inference_budget_contract::RootIdentity,
    limits: crate::inference_budget_contract::Limits,
    catalog: &k8s_openapi::api::core::v1::ConfigMap,
    source: &KarsSandbox,
    accounting_namespace: &str,
) -> Result<BuiltinFunding, StoreError> {
    if catalog.uid().as_deref() != Some(config_uid)
        || catalog.metadata.deletion_timestamp.is_some()
        || catalog
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || source
            .annotations()
            .contains_key(super::first_install::ANNOTATION)
    {
        return Err(BudgetError::Authorization.into());
    }
    let originals = parse_config(
        catalog
            .data
            .as_ref()
            .and_then(|data| data.get(CONFIG_KEY))
            .map(String::as_str)
            .unwrap_or("[]"),
    )?;
    let original = originals
        .iter()
        .find(|original| original.root.resource.name == "bridge-orchestrator")
        .ok_or(BudgetError::Authorization)?;
    verify_source(original, source, i64::MIN)?;
    if original.root != *root
        || original.limits.normalized() != limits
        || source.spec.inference_budget_ref.as_ref()
            != Some(&binding(
                original,
                AccountReference {
                    namespace: accounting_namespace.into(),
                    name: super::account::name_for_root(&original.root),
                    uid: account_uid.into(),
                },
            )?)
    {
        return Err(BudgetError::Authorization.into());
    }
    Ok(original.clone())
}

pub(super) async fn verify_dispatch_grant(
    client: &Client,
    ledger: &Ledger,
    sandbox: &KarsSandbox,
) -> Result<(), StoreError> {
    let grant = configured(client, sandbox)
        .await?
        .ok_or(BudgetError::Authorization)?;
    if grant.unbounded_tokens && ledger.unbounded_tokens.is_none() {
        return Err(BudgetError::Authorization.into());
    }
    verify_grant(&grant, ledger, sandbox, chrono::Utc::now().timestamp())
}

pub(super) async fn recover(
    client: &Client,
    store: &Store,
    account: &super::account::KarsBudgetAccount,
) -> Result<(), StoreError> {
    let root = &account.spec.root;
    let uid = account.uid().ok_or(BudgetError::Identity)?;
    let sandbox = Api::<KarsSandbox>::namespaced(client.clone(), &root.resource.namespace)
        .get_opt(&root.resource.name)
        .await
        .map_err(api_error)?;
    let valid = if let Some(sandbox) = sandbox {
        match configured(client, &sandbox).await? {
            Some(grant) => {
                // Configured funding can have CAS-applied a renewal; never close
                // the account using the pre-renewal scan snapshot.
                let current = store.read(root, &uid).await?;
                let ledger = Store::ledger(&current)?;
                verify_source(&grant, &sandbox, i64::MIN).is_ok()
                    && ledger
                        .validate_funding_expiry(&grant, chrono::Utc::now().timestamp())
                        .is_ok()
                    && grant.root == *root
                    && grant.limits.normalized() == ledger.limits
                    && sandbox.spec.inference_budget_ref.as_ref()
                        == Some(&binding(
                            &grant,
                            AccountReference {
                                namespace: account.namespace().ok_or(BudgetError::Identity)?,
                                name: account.name_any(),
                                uid: uid.clone(),
                            },
                        )?)
                    && (ledger.nodes.is_empty()
                        || ledger.nodes.get(&root.resource.uid).is_some_and(|node| {
                            authority(&grant).is_ok_and(|authority| authority == node.authority)
                        }))
            }
            _ => false,
        }
    } else {
        false
    };
    if !valid {
        store
            .transact(root, &uid, |ledger| ledger.close_account())
            .await?;
        return Ok(());
    }
    for session in Store::ledger(account)?
        .sessions
        .values()
        .filter(|session| !session.closed)
    {
        let pods: Api<Pod> =
            Api::namespaced(client.clone(), &format!("kars-{}", root.resource.name));
        let pod = pods
            .get_opt(&session.identity.pod_name)
            .await
            .map_err(api_error)?;
        if pod.is_none_or(|pod| {
            pod.uid().as_deref() != Some(&session.identity.pod_uid)
                || pod.metadata.deletion_timestamp.is_some()
        }) {
            store
                .transact(root, &uid, |ledger| {
                    ledger.close_session(&session.identity.pod_uid)
                })
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_budget_contract::{Limits, ResourceIdentity, RootIdentity};

    fn source(name: &str) -> (BuiltinFunding, KarsSandbox) {
        let mut sandbox = KarsSandbox::new(name, Default::default());
        sandbox.metadata.namespace = Some("workspace".into());
        sandbox.metadata.uid = Some("sandbox-uid".into());
        sandbox.metadata.resource_version = Some("1".into());
        sandbox.spec.runtime.kind = RuntimeKind::Hermes;
        let grant = BuiltinFunding {
            root: RootIdentity {
                kind: RootKind::KarsSandbox,
                resource: ResourceIdentity {
                    namespace: "workspace".into(),
                    name: name.into(),
                    uid: "sandbox-uid".into(),
                },
                workspace_uid: "workspace-uid".into(),
                cluster_uid: "cluster-uid".into(),
            },
            limits: Limits {
                tokens: Some(100),
                usd_micros: None,
            },
            expires_at: "2030-01-01T00:00:00Z".into(),
            unbounded_tokens: false,
        };
        (grant, sandbox)
    }

    fn bridge_renewal_fixture() -> (
        crate::inference_budget_contract::ledger::TokenTopUp,
        k8s_openapi::api::core::v1::ConfigMap,
        KarsSandbox,
    ) {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../shared/inference_budget/bridge-renewal-fixture.json"
        ))
        .unwrap();
        let grant: crate::inference_budget_contract::ledger::TokenTopUp =
            serde_json::from_value(value["grant"].clone()).unwrap();
        let original: BuiltinFunding = serde_json::from_value(
            value["ledger"]["nodes"]["sandbox-uid"]["authority"]["effectiveAuthorization"].clone(),
        )
        .unwrap();
        let (_, mut sandbox) = source("bridge-orchestrator");
        sandbox.spec.inference_budget_ref = Some(
            binding(
                &original,
                AccountReference {
                    namespace: "workspace".into(),
                    name: super::super::account::name_for_root(&grant.root),
                    uid: grant.account_uid.clone(),
                },
            )
            .unwrap(),
        );
        let catalog = serde_json::from_value(json!({
            "metadata": {"name":"contracts", "namespace":"workspace", "uid":"config-uid", "resourceVersion":"1",
                "annotations":{"kars.azure.com/inference-budget-contracts":"v1"}},
            "data": {CONFIG_KEY: serde_json::to_string(&vec![original]).unwrap()}
        })).unwrap();
        (grant, catalog, sandbox)
    }

    #[test]
    fn bridge_renewal_requires_unchanged_original_catalog_source_and_account_bindings() {
        let (grant, catalog, sandbox) = bridge_renewal_fixture();
        verify_bridge_renewal(&grant, &catalog, &sandbox, "workspace").unwrap();
        for fault in [
            "catalog-uid",
            "catalog-delete",
            "catalog-rv",
            "revoked",
            "changed-grant",
            "duplicate-grant",
            "source-uid",
            "source-delete",
            "source-owner",
            "source-name",
            "source-namespace",
            "source-rv",
            "sre-intent",
            "account-uid",
            "account-namespace",
            "account-name",
            "binding-digest",
            "grant-digest",
            "root",
            "expiry",
            "no-consent",
        ] {
            let mut proposed = grant.clone();
            let mut map = serde_json::to_value(&catalog).unwrap();
            let mut source = serde_json::to_value(&sandbox).unwrap();
            match fault {
                "catalog-uid" => map["metadata"]["uid"] = json!("replacement"),
                "catalog-delete" => {
                    map["metadata"]["deletionTimestamp"] = json!("2026-10-20T00:00:00Z")
                }
                "catalog-rv" => map["metadata"]["resourceVersion"] = json!(""),
                "revoked" => map["data"][CONFIG_KEY] = json!("[]"),
                "changed-grant" | "duplicate-grant" => {
                    let mut originals: Vec<serde_json::Value> =
                        serde_json::from_str(map["data"][CONFIG_KEY].as_str().unwrap()).unwrap();
                    if fault == "changed-grant" {
                        originals[0]["limits"]["tokens"] = json!(101);
                    } else {
                        originals.push(originals[0].clone());
                    }
                    map["data"][CONFIG_KEY] = json!(serde_json::to_string(&originals).unwrap());
                }
                "source-uid" => source["metadata"]["uid"] = json!("replacement"),
                "source-delete" => {
                    source["metadata"]["deletionTimestamp"] = json!("2026-10-20T00:00:00Z")
                }
                "source-owner" => {
                    source["metadata"]["ownerReferences"] = json!([{
                        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask", "name":"owner", "uid":"owner-uid"
                    }])
                }
                "source-name" => source["metadata"]["name"] = json!("another"),
                "source-namespace" => source["metadata"]["namespace"] = json!("another"),
                "source-rv" => source["metadata"]["resourceVersion"] = json!(""),
                "sre-intent" => {
                    source["metadata"]["annotations"] =
                        json!({(super::super::first_install::ANNOTATION): "sre-intent"})
                }
                "account-uid" => {
                    source["spec"]["inferenceBudgetRef"]["account"]["uid"] = json!("replacement")
                }
                "account-namespace" => {
                    source["spec"]["inferenceBudgetRef"]["account"]["namespace"] = json!("another")
                }
                "account-name" => {
                    source["spec"]["inferenceBudgetRef"]["account"]["name"] = json!("another")
                }
                "binding-digest" => {
                    source["spec"]["inferenceBudgetRef"]["authorizationDigest"] =
                        json!(format!("sha256:{}", "a".repeat(64)))
                }
                "grant-digest" => {
                    proposed
                        .nodes
                        .insert("sandbox-uid".into(), format!("sha256:{}", "a".repeat(64)));
                }
                "root" => proposed.root.cluster_uid = "replacement".into(),
                "expiry" => {
                    proposed
                        .funding_renewal
                        .as_mut()
                        .unwrap()
                        .original_expires_at = "2021-01-01T00:00:00Z".into()
                }
                "no-consent" => {
                    proposed
                        .funding_renewal
                        .as_mut()
                        .unwrap()
                        .bridge_reactivation = None
                }
                _ => unreachable!(),
            }
            assert!(
                verify_bridge_renewal(
                    &proposed,
                    &serde_json::from_value(map).unwrap(),
                    &serde_json::from_value(source).unwrap(),
                    "workspace"
                )
                .is_err(),
                "{fault}"
            );
        }
    }

    #[tokio::test]
    async fn bridge_renewal_rechecks_live_source_without_sre_registration_on_each_call() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = MockServer::start().await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let settings = Settings {
            accounting_namespace: "workspace".into(),
            catalog_name: "contracts".into(),
            address: "0.0.0.0:9447".into(),
            tls_secret: "budget-tls".into(),
            router_image_digest: format!("sha256:{}", "a".repeat(64)),
            first_install_router_digest: None,
        };
        let (grant, catalog, source) = bridge_renewal_fixture();
        let grants = std::collections::BTreeMap::from([(grant.id.clone(), grant.clone())]);
        for change in [
            "none",
            "replacement",
            "deleting",
            "unavailable",
            "stale-root",
            "stale-account",
        ] {
            server.reset().await;
            let mut current = source.clone();
            if change == "replacement" {
                current.metadata.uid = Some("replacement".into());
            }
            if change == "deleting" {
                current.metadata.deletion_timestamp = Some(
                    k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                        k8s_openapi::jiff::Timestamp::now(),
                    ),
                );
            }
            let response = if change == "unavailable" {
                ResponseTemplate::new(503).set_body_json(json!({"apiVersion":"v1", "kind":"Status",
                    "status":"Failure", "reason":"ServiceUnavailable", "message":"unavailable", "code":503}))
            } else {
                ResponseTemplate::new(200).set_body_json(&current)
            };
            Mock::given(method("GET")).and(path("/apis/kars.azure.com/v1alpha1/namespaces/workspace/karssandboxes/bridge-orchestrator"))
                .respond_with(response).expect(1).mount(&server).await;
            let mut root = grant.root.clone();
            if change == "stale-root" {
                root.resource.uid = "replacement".into();
            }
            let account = if change == "stale-account" {
                "replacement"
            } else {
                grant.account_uid.as_str()
            };
            let result = super::super::first_install::verify_renewal_intents(
                &client, &settings, &catalog, &grants, &root, account,
            )
            .await;
            assert_eq!(result.is_ok(), change == "none", "{change}");
            server.verify().await;
        }
    }

    #[test]
    fn builtin_operator_intent_rejects_ambiguity_unbounded_limits_and_non_builtin_roots() {
        let (grant, _) = source("sre");
        assert_eq!(
            parse_config(&json!([grant]).to_string()).unwrap(),
            vec![grant.clone()]
        );
        assert!(parse_config(&json!([grant, grant]).to_string()).is_err());
        assert!(parse_config("{}").is_err());
        for change in [
            "unknown", "zero", "missing", "overflow", "kind", "name", "expiry",
        ] {
            let mut value = json!([grant]);
            match change {
                "unknown" => value[0]["dailyTokens"] = json!(100),
                "zero" => value[0]["limits"]["tokens"] = json!(0),
                "missing" => value[0]["limits"] = json!({"usdMicros": 100}),
                "overflow" => value[0]["limits"]["tokens"] = json!(u64::MAX),
                "kind" => value[0]["root"]["kind"] = json!("KarsTask"),
                "name" => value[0]["root"]["resource"]["name"] = json!("another-actor"),
                "expiry" => value[0]["expiresAt"] = json!("tomorrow"),
                _ => unreachable!(),
            }
            assert!(parse_config(&value.to_string()).is_err(), "{change}");
        }
    }

    #[tokio::test]
    async fn builtin_funding_reads_current_operator_configuration_and_refuses_read_failures() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = MockServer::start().await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let settings = Settings {
            accounting_namespace: "workspace".into(),
            catalog_name: "contracts".into(),
            address: "0.0.0.0:9447".into(),
            tls_secret: "budget-tls".into(),
            router_image_digest: format!("sha256:{}", "a".repeat(64)),
            first_install_router_digest: None,
        };
        let (grant, sandbox) = source("sre");
        for mutation in [
            "initial",
            "changed",
            "removed",
            "absent",
            "invalid",
            "ambiguous",
            "uid",
            "resource-version",
            "annotation",
            "deleting",
            "unavailable",
        ] {
            server.reset().await;
            let mut current = grant.clone();
            if mutation == "changed" {
                current.limits.tokens = Some(200);
            }
            let mut config = json!({
                "apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {
                    "name": "contracts", "namespace": "workspace", "uid": "config-uid",
                    "resourceVersion": "7",
                    "annotations": {"kars.azure.com/inference-budget-contracts": "v1"}
                },
                "data": {CONFIG_KEY: serde_json::to_string(&vec![current.clone()]).unwrap()}
            });
            match mutation {
                "removed" => config["data"][CONFIG_KEY] = json!("[]"),
                "absent" => config["data"] = json!({}),
                "invalid" => config["data"][CONFIG_KEY] = json!("{"),
                "ambiguous" => {
                    config["data"][CONFIG_KEY] =
                        json!(serde_json::to_string(&vec![grant.clone(), grant.clone()]).unwrap());
                }
                "uid" => config["metadata"]["uid"] = json!(""),
                "resource-version" => config["metadata"]["resourceVersion"] = json!(""),
                "annotation" => config["metadata"]["annotations"] = json!({}),
                "deleting" => {
                    config["metadata"]["deletionTimestamp"] = json!("2026-10-13T00:00:00Z");
                }
                _ => {}
            }
            let response = if mutation == "unavailable" {
                ResponseTemplate::new(503).set_body_json(json!({
                    "apiVersion": "v1", "kind": "Status", "status": "Failure",
                    "reason": "ServiceUnavailable", "message": "unavailable", "code": 503
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(config)
            };
            Mock::given(method("GET"))
                .and(path("/api/v1/namespaces/workspace/configmaps/contracts"))
                .respond_with(response)
                .expect(1)
                .mount(&server)
                .await;
            let result = configured_from_store(&client, &settings, &sandbox).await;
            match mutation {
                "initial" | "changed" => assert_eq!(result.unwrap(), Some(current)),
                "removed" | "absent" => assert_eq!(result.unwrap(), None),
                _ => assert!(result.is_err(), "{mutation}"),
            }
            server.verify().await;
        }
    }

    #[test]
    fn builtin_source_is_never_adopted_by_name_or_repurposed_as_a_task() {
        for name in ["sre", "bridge-orchestrator"] {
            let (grant, sandbox) = source(name);
            assert!(verify_source(&grant, &sandbox, 100).is_ok());
            for field in ["uid", "namespace", "name"] {
                let mut changed = sandbox.clone();
                match field {
                    "uid" => changed.metadata.uid = Some("replacement".into()),
                    "namespace" => changed.metadata.namespace = Some("foreign".into()),
                    "name" => changed.metadata.name = Some("other".into()),
                    _ => unreachable!(),
                }
                assert!(verify_source(&grant, &changed, 100).is_err());
            }
            let mut owned = sandbox;
            owned.metadata.owner_references = Some(vec![
                serde_json::from_value(json!({
                    "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTask",
                    "name":"task", "uid":"real-task-uid", "controller":true
                }))
                .unwrap(),
            ]);
            assert!(verify_source(&grant, &owned, 100).is_err());
        }
        let (grant, mut sandbox) = source("sre");
        sandbox.spec.runtime.kind = RuntimeKind::OpenClaw;
        assert!(verify_source(&grant, &sandbox, 100).is_err());
    }

    #[tokio::test]
    async fn builtin_sre_requires_the_current_registered_source_and_privacy_epoch() {
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let server = MockServer::start().await;
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let (_, sandbox) = source("sre");
        let registration: crate::sre_registration::KarsSRERegistration = serde_json::from_value(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSRERegistration",
            "metadata":{"name":"canonical","uid":"registration-uid","generation":1},
            "spec":{
                "controller":{
                    "namespace":{"name":"workspace","uid":"workspace-uid"},
                    "deployment":{"name":"kars-controller","uid":"controller-uid"},"release":"kars"
                },
                "sandbox":{"namespace":"workspace","name":"sre","uid":"sandbox-uid"},
                "runtimeNamespace":{"name":"kars-sre","uid":"runtime-uid"},
                "enabled":true
            }
        })).unwrap();
        let epoch = Some(registration.epoch());
        for mutation in ["none", "uid", "disabled", "missing"] {
            server.reset().await;
            let mut changed = registration.clone();
            match mutation {
                "uid" => changed.spec.sandbox.uid = "replacement".into(),
                "disabled" => changed.spec.enabled = false,
                _ => {}
            }
            let response = if mutation == "missing" {
                ResponseTemplate::new(404).set_body_json(json!({
                    "apiVersion":"v1","kind":"Status","status":"Failure",
                    "reason":"NotFound","message":"missing","code":404
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(changed)
            };
            Mock::given(method("GET"))
                .and(path(
                    "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical",
                ))
                .respond_with(response)
                .mount(&server)
                .await;
            assert_eq!(
                verify_registration(&client, &sandbox, "runtime-uid", &epoch)
                    .await
                    .is_ok(),
                mutation == "none",
            );
            assert!(
                verify_registration(&client, &sandbox, "runtime-uid", &None)
                    .await
                    .is_err()
            );
            assert!(
                verify_registration(&client, &sandbox, "replacement", &epoch)
                    .await
                    .is_err()
            );
        }
    }

    #[test]
    fn builtin_migration_refuses_unknown_or_calendar_only_history() {
        let (_, sandbox) = source("sre");
        let mut namespace = Namespace::default();
        assert!(verify_fresh_history(&sandbox, &namespace, false).is_err());
        namespace.annotations_mut().insert(
            super::super::calendar::INITIALIZATION_ANNOTATION.into(),
            "sandbox-uid".into(),
        );
        assert!(verify_fresh_history(&sandbox, &namespace, false).is_ok());
        assert!(verify_fresh_history(&sandbox, &namespace, true).is_err());
        namespace.annotations_mut().insert(
            crate::inference_budget_contract::calendar::ACCOUNT_ANNOTATION.into(),
            "old-calendar".into(),
        );
        assert!(verify_fresh_history(&sandbox, &namespace, false).is_err());
    }

    #[test]
    fn builtin_authorization_is_immutable_expiring_and_keeps_settlement_identity() {
        let (grant, mut sandbox) = source("sre");
        let authority = authority(&grant).unwrap();
        let mut ledger = Ledger::new("account-uid".into(), grant.root.clone(), grant.limits)
            .unwrap()
            .register_task(authority.clone())
            .unwrap()
            .next;
        assert!(verify_grant(&grant, &ledger, &sandbox, 100).is_ok());
        assert!(verify_grant(&grant, &ledger, &sandbox, 1_893_456_000).is_err());
        for field in ["expiry", "limit", "cluster", "workspace"] {
            let mut changed = grant.clone();
            match field {
                "expiry" => changed.expires_at = "2031-01-01T00:00:00Z".into(),
                "limit" => changed.limits.tokens = Some(200),
                "cluster" => changed.root.cluster_uid = "new-cluster".into(),
                "workspace" => changed.root.workspace_uid = "new-workspace".into(),
                _ => unreachable!(),
            }
            assert!(
                verify_grant(&changed, &ledger, &sandbox, 100).is_err(),
                "{field}"
            );
        }
        sandbox.spec.inference_budget_ref = Some(
            binding(
                &grant,
                AccountReference {
                    namespace: "accounting".into(),
                    name: super::super::account::name_for_root(&grant.root),
                    uid: "account-uid".into(),
                },
            )
            .unwrap(),
        );
        let identity = ExecutionIdentity {
            task_uid: "sandbox-uid".into(),
            authorization_digest: authority.authorization_digest,
            sandbox: grant.root.resource.clone(),
            runtime_namespace_uid: "runtime-uid".into(),
            pod_name: "router".into(),
            pod_uid: "pod-uid".into(),
        };
        ledger = ledger
            .register_session(identity.clone())
            .unwrap()
            .next
            .close_account()
            .unwrap()
            .next;
        assert!(verify_binding(&ledger, &sandbox, &identity, false).is_ok());
        let mut replacement = identity.clone();
        replacement.pod_uid = "replacement".into();
        assert!(verify_binding(&ledger, &sandbox, &replacement, false).is_err());
        sandbox
            .spec
            .inference_budget_ref
            .as_mut()
            .unwrap()
            .account
            .uid = "new-account".into();
        assert!(verify_binding(&ledger, &sandbox, &identity, false).is_err());
    }
}
