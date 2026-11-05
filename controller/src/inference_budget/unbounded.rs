// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{config::Settings, store::StoreError};
use crate::{
    crd::KarsSandbox,
    inference_budget_contract::{
        AccountReference, BudgetError, BuiltinFunding, RootIdentity, RootKind,
        ledger::{Ledger, UnboundedTokenAllowance},
    },
    kars_task::KarsTask,
    kars_team::KarsTeam,
    sre_registration::KarsSRERegistration,
};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{Api, Client};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const CONFIG_KEY: &str = "unbounded-token-allowances.json";
pub(super) type Allowances = BTreeMap<String, UnboundedTokenAllowance>;

#[cfg(test)]
#[path = "unbounded_tests.rs"]
mod tests;

pub(super) fn configured(map: &ConfigMap) -> Result<Allowances, BudgetError> {
    let uid = map.metadata.uid.as_deref().ok_or(BudgetError::Identity)?;
    let encoded = map
        .data
        .as_ref()
        .and_then(|data| data.get(CONFIG_KEY))
        .map(String::as_str)
        .unwrap_or("[]");
    if encoded.len() > 262_144 {
        return Err(BudgetError::Capacity);
    }
    let entries: Vec<UnboundedTokenAllowance> =
        serde_json::from_str(encoded).map_err(|_| BudgetError::Authorization)?;
    let mut grants = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for grant in entries {
        grant.validate()?;
        if grant.config_uid != uid
            || !ids.insert(grant.id.clone())
            || grants.insert(grant.account_uid.clone(), grant).is_some()
        {
            return Err(BudgetError::Authorization);
        }
    }
    Ok(grants)
}

pub(super) fn validate_receipt(ledger: &Ledger, grants: &Allowances) -> Result<(), BudgetError> {
    ledger.validate()?;
    if let Some(receipt) = &ledger.unbounded_tokens
        && grants.get(&ledger.account_uid) != Some(receipt)
    {
        return Err(BudgetError::Authorization);
    }
    Ok(())
}

pub(super) fn apply(ledger: &Ledger, grants: &Allowances, now: i64) -> Result<Ledger, BudgetError> {
    validate_receipt(ledger, grants)?;
    match grants.get(&ledger.account_uid) {
        Some(grant) => Ok(ledger.authorize_unbounded_tokens(grant, now)?.next),
        None => Ok(ledger.clone()),
    }
}

/// The authenticated catalog is necessary but not sufficient: a replacement
/// root, registration, namespace or binding never inherits an old allowance.
pub(super) async fn verify_sources(
    client: &Client,
    settings: &Settings,
    catalog: &ConfigMap,
    grants: &Allowances,
    root: &RootIdentity,
    account_uid: &str,
) -> Result<Option<BuiltinFunding>, StoreError> {
    let relevant: Vec<_> = grants
        .values()
        .filter(|grant| {
            grant.account_uid == account_uid
                || (grant.root.kind == root.kind
                    && grant.root.resource.namespace == root.resource.namespace
                    && grant.root.resource.name == root.resource.name)
        })
        .collect();
    if relevant.is_empty() {
        return Ok(None);
    }
    if relevant.len() != 1 || relevant[0].account_uid != account_uid || relevant[0].root != *root {
        return Err(BudgetError::Identity.into());
    }
    let grant = relevant[0];
    let namespaces: Api<Namespace> = Api::all(client.clone());
    for (name, uid) in [
        ("kube-system", &root.cluster_uid),
        (root.resource.namespace.as_str(), &root.workspace_uid),
    ] {
        let live = namespaces
            .get(name)
            .await
            .map_err(|_| BudgetError::Identity)?;
        if live.metadata.uid.as_ref() != Some(uid) || live.metadata.deletion_timestamp.is_some() {
            return Err(BudgetError::Identity.into());
        }
    }
    let expected = AccountReference {
        namespace: settings.accounting_namespace.clone(),
        name: super::account::name_for_root(root),
        uid: account_uid.into(),
    };
    match root.kind {
        RootKind::KarsTask => {
            let live = Api::<KarsTask>::namespaced(client.clone(), &root.resource.namespace)
                .get(&root.resource.name)
                .await
                .map_err(|_| BudgetError::Identity)?;
            let binding = live
                .status
                .as_ref()
                .and_then(|status| status.inference_budget.as_ref())
                .ok_or(BudgetError::Authorization)?;
            if live.metadata.uid.as_ref() != Some(&root.resource.uid)
                || live.metadata.deletion_timestamp.is_some()
                || binding.account != expected
                || binding.root != *root
                || binding.task_uid != root.resource.uid
                || super::binding::limits(&live.spec.envelope)? != grant.original_limits
            {
                return Err(BudgetError::Identity.into());
            }
            Ok(None)
        }
        RootKind::KarsTeam => {
            let live = Api::<KarsTeam>::namespaced(client.clone(), &root.resource.namespace)
                .get(&root.resource.name)
                .await
                .map_err(|_| BudgetError::Identity)?;
            if live.metadata.uid.as_ref() != Some(&root.resource.uid)
                || live.metadata.deletion_timestamp.is_some()
                || live
                    .status
                    .as_ref()
                    .and_then(|status| status.inference_budget_account.as_ref())
                    != Some(&expected)
                || super::binding::limits(&live.spec.envelope)? != grant.original_limits
            {
                return Err(BudgetError::Identity.into());
            }
            Ok(None)
        }
        RootKind::KarsSandbox => {
            let source = Api::<KarsSandbox>::namespaced(client.clone(), &root.resource.namespace)
                .get(&root.resource.name)
                .await
                .map_err(|_| BudgetError::Identity)?;
            let original = if root.resource.name == "bridge-orchestrator" {
                super::builtin::verify_bridge_funding_source(
                    &grant.config_uid,
                    account_uid,
                    root,
                    grant.original_limits,
                    catalog,
                    &source,
                    &settings.accounting_namespace,
                )?
            } else {
                let reg = Api::<KarsSRERegistration>::all(client.clone())
                    .get(crate::sre_registration::NAME)
                    .await
                    .map_err(|_| BudgetError::Identity)?;
                super::first_install::validate(client, settings, &source, catalog, &reg).await?
            };
            if original.root != *root
                || original.limits.normalized() != grant.original_limits
                || source.spec.inference_budget_ref.as_ref()
                    != Some(&super::builtin::binding(&original, expected)?)
            {
                return Err(BudgetError::Identity.into());
            }
            Ok(Some(original))
        }
    }
}

pub(super) fn verify_original(
    ledger: &Ledger,
    original: Option<&BuiltinFunding>,
) -> Result<(), BudgetError> {
    if let Some(original) = original {
        let node = ledger
            .nodes
            .get(&ledger.root.resource.uid)
            .ok_or(BudgetError::Identity)?;
        if node.authority
            != super::builtin::authority(original).map_err(|_| BudgetError::Authorization)?
        {
            return Err(BudgetError::Authorization);
        }
    }
    Ok(())
}
