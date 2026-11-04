// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    config::Settings,
    store::{Store, StoreError},
};
use crate::inference_budget_contract::{
    BudgetError, RootIdentity,
    ledger::{Ledger, Mutation, TokenTopUp},
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::Client;
use std::collections::BTreeMap;

pub const CONFIG_KEY: &str = "token-top-ups.json";
const MAX_CONFIG_BYTES: usize = 262_144;

fn configured(map: &ConfigMap) -> Result<BTreeMap<String, TokenTopUp>, BudgetError> {
    let uid = map.metadata.uid.as_deref().ok_or(BudgetError::Identity)?;
    let encoded = map
        .data
        .as_ref()
        .and_then(|data| data.get(CONFIG_KEY))
        .map(String::as_str)
        .unwrap_or("[]");
    if encoded.len() > MAX_CONFIG_BYTES {
        return Err(BudgetError::Capacity);
    }
    let values: Vec<TokenTopUp> =
        serde_json::from_str(encoded).map_err(|_| BudgetError::Authorization)?;
    let mut grants = BTreeMap::new();
    for grant in values {
        // An expired grant for another account must not block this account.
        grant.validate(i64::MIN)?;
        if grant.config_uid != uid || grants.insert(grant.id.clone(), grant).is_some() {
            return Err(BudgetError::Authorization);
        }
    }
    Ok(grants)
}

fn apply(
    ledger: &Ledger,
    grants: &BTreeMap<String, TokenTopUp>,
    now: i64,
) -> Result<Ledger, BudgetError> {
    ledger.validate()?;
    validate_receipts(ledger, grants, i64::MIN)?;
    let mut next = ledger.clone();
    let relevant: Vec<_> = grants
        .values()
        .filter(|grant| {
            grant.account_uid == ledger.account_uid
                || (grant.root.kind == ledger.root.kind
                    && grant.root.resource.namespace == ledger.root.resource.namespace
                    && grant.root.resource.name == ledger.root.resource.name)
        })
        .collect();
    let mut renewals: Vec<_> = relevant
        .iter()
        .copied()
        .filter(|grant| {
            grant.funding_renewal.is_some() && !ledger.token_top_ups.contains_key(&grant.id)
        })
        .collect();
    renewals.sort_by_key(|grant| {
        grant
            .funding_renewal
            .as_ref()
            .unwrap()
            .previous_top_up_ids
            .len()
    });
    for grant in renewals {
        next = next.apply_token_top_up(grant, now)?.next;
    }
    validate_receipts(&next, grants, now)?;
    for grant in relevant {
        next = next.apply_token_top_up(grant, now)?.next;
    }
    Ok(next)
}

fn validate_receipts(
    ledger: &Ledger,
    grants: &BTreeMap<String, TokenTopUp>,
    now: i64,
) -> Result<(), BudgetError> {
    // Revoked receipts remain liability evidence, but never authorize new work.
    for (id, receipt) in &ledger.token_top_ups {
        if grants.get(id) != Some(receipt) {
            return Err(BudgetError::Authorization);
        }
        ledger.validate_top_up_expiry(receipt, now)?;
    }
    Ok(())
}

pub(super) async fn authorize_read(
    client: &Client,
    settings: &Settings,
    ledger: &Ledger,
) -> Result<(), StoreError> {
    if !ledger.token_top_ups.is_empty() || ledger.unbounded_tokens.is_some() {
        super::admission::verify(client, &settings.accounting_namespace).await?;
        let catalog = settings.operator_config(client).await?;
        let grants = configured(&catalog)?;
        let allowances = super::unbounded::configured(&catalog)?;
        super::unbounded::validate_receipt(ledger, &allowances)?;
        let original = super::unbounded::verify_sources(
            client, settings, &catalog, &allowances, &ledger.root, &ledger.account_uid,
        ).await?;
        super::unbounded::verify_original(ledger, original.as_ref())?;
        super::first_install::verify_renewal_intents(
            client,
            settings,
            &catalog,
            &grants,
            &ledger.root,
            &ledger.account_uid,
        )
        .await?;
        validate_receipts(ledger, &grants, chrono::Utc::now().timestamp())?;
    }
    Ok(())
}

/// Only operator-owned ConfigMap entries are consumed. Workload requests never
/// contain top-ups. Each retry validates admission and re-reads ConfigMap UID/RV.
pub(super) async fn transact<T>(
    client: &Client,
    settings: &Settings,
    store: &Store,
    root: &RootIdentity,
    account_uid: &str,
    operation: impl Fn(&Ledger) -> Result<Mutation<T>, BudgetError>,
) -> Result<T, StoreError> {
    store
        .transact_prepared(
            root,
            account_uid,
            || async {
                super::admission::verify(client, &settings.accounting_namespace).await?;
                let catalog = settings.operator_config(client).await?;
                let grants = configured(&catalog)?;
                let allowances = super::unbounded::configured(&catalog)?;
                let original = super::unbounded::verify_sources(
                    client, settings, &catalog, &allowances, root, account_uid,
                ).await?;
                super::first_install::verify_renewal_intents(
                    client,
                    settings,
                    &catalog,
                    &grants,
                    root,
                    account_uid,
                )
                .await?;
                if grants
                    .values()
                    .any(|grant| grant.account_uid == account_uid)
                {
                    let namespaces: kube::Api<k8s_openapi::api::core::v1::Namespace> =
                        kube::Api::all(client.clone());
                    for (name, uid) in [
                        ("kube-system", &root.cluster_uid),
                        (root.resource.namespace.as_str(), &root.workspace_uid),
                    ] {
                        let namespace =
                            namespaces
                                .get(name)
                                .await
                                .map_err(|error| StoreError::Api {
                                    stage: "verify top-up cluster/workspace incarnation",
                                    code: match error {
                                        kube::Error::Api(status) => Some(status.code),
                                        _ => None,
                                    },
                                })?;
                        if namespace.metadata.uid.as_ref() != Some(uid)
                            || namespace.metadata.deletion_timestamp.is_some()
                        {
                            return Err(BudgetError::Identity.into());
                        }
                    }
                }
                Ok((grants, allowances, original))
            },
            |ledger, (grants, allowances, original)| {
                let now = chrono::Utc::now().timestamp();
                super::unbounded::verify_original(ledger, original.as_ref())?;
                validate_receipts(ledger, &grants, i64::MIN)?;
                let allowed = super::unbounded::apply(ledger, &allowances, now)?;
                let funded = apply(&allowed, &grants, now)?;
                let mut result = operation(&funded)?;
                result.changed = result.next != *ledger;
                Ok(result)
            },
        )
        .await
}

pub(super) async fn reconcile(
    client: &Client,
    settings: &Settings,
    store: &Store,
    root: &RootIdentity,
    account_uid: &str,
) -> Result<(), StoreError> {
    transact(client, settings, store, root, account_uid, |ledger| {
        Ok(Mutation {
            next: ledger.clone(),
            value: (),
            changed: false,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_budget_contract::{Limits, ResourceIdentity, RootKind, TaskAuthority};
    use serde_json::json;

    fn fixture() -> (Ledger, TokenTopUp, ConfigMap) {
        let root = RootIdentity {
            kind: RootKind::KarsTeam,
            resource: ResourceIdentity {
                namespace: "workspace".into(),
                name: "team".into(),
                uid: "team-uid".into(),
            },
            workspace_uid: "workspace-uid".into(),
            cluster_uid: "cluster-uid".into(),
        };
        let mut task = root.resource.clone();
        task.uid = "principal-uid".into();
        let digest = format!("sha256:{}", "a".repeat(64));
        let limits = Limits {
            tokens: Some(100),
            usd_micros: None,
        };
        let ledger = Ledger::new("account-uid".into(), root.clone(), limits)
            .unwrap()
            .register_task(TaskAuthority {
                task: task.clone(),
                parent_uid: None,
                root_task_uid: task.uid.clone(),
                authorization_digest: digest.clone(),
                effective_authorization: json!({}),
                limits,
            })
            .unwrap()
            .next;
        let grant = TokenTopUp {
            funding_renewal: None,
            id: "extension-1".into(),
            config_uid: "config-uid".into(),
            account_uid: ledger.account_uid.clone(),
            root,
            original_limits: limits,
            tokens: 100,
            expires_at: "2030-01-01T00:00:00Z".into(),
            nodes: BTreeMap::from([(task.uid, digest)]),
        };
        let map = serde_json::from_value(json!({"metadata": {"uid": "config-uid"},
            "data": {CONFIG_KEY: serde_json::to_string(&vec![&grant]).unwrap()}}))
        .unwrap();
        (ledger, grant, map)
    }

    #[test]
    fn operator_source_and_receipts_fail_closed_without_erasing_liability() {
        let (ledger, grant, mut map) = fixture();
        let grants = configured(&map).unwrap();
        let funded = apply(&ledger, &grants, 100).unwrap();
        assert_eq!(apply(&funded, &grants, 100).unwrap(), funded);
        assert!(apply(&funded, &BTreeMap::new(), 100).is_err());
        assert!(apply(&funded, &grants, i64::MAX).is_err());
        let mut changed = grants.clone();
        changed.get_mut(&grant.id).unwrap().tokens += 1;
        assert!(apply(&funded, &changed, 100).is_err());
        map.metadata.uid = Some("replacement".into());
        assert!(configured(&map).is_err());
        funded.validate().unwrap();
        assert_eq!(funded.meters, ledger.meters);
        assert_eq!(funded.limits, ledger.limits);
    }

    #[test]
    fn orchestrator_top_up_uses_operator_receipts_and_preserves_original_expiry() {
        let (mut ledger, mut grant, mut map) = fixture();
        ledger.root.kind = RootKind::KarsSandbox;
        ledger.root.resource.name = "bridge-orchestrator".into();
        ledger.root.resource.uid = "principal-uid".into();
        let original = crate::inference_budget_contract::BuiltinFunding {
            root: ledger.root.clone(),
            limits: ledger.limits,
            expires_at: "1970-01-01T00:08:20Z".into(),
            unbounded_tokens: false,
        };
        let authority = &mut ledger.nodes.get_mut("principal-uid").unwrap().authority;
        authority.task = ledger.root.resource.clone();
        authority.effective_authorization = serde_json::to_value(&original).unwrap();
        grant.root = ledger.root.clone();
        map.data.as_mut().unwrap().insert(
            CONFIG_KEY.into(),
            serde_json::to_string(&vec![&grant]).unwrap(),
        );
        let grants = configured(&map).unwrap();
        let funded = apply(&ledger, &grants, 100).unwrap();
        assert_eq!(funded.effective_limits().unwrap().tokens, Some(200));
        assert_eq!(funded.nodes, ledger.nodes);
        assert_eq!(funded.meters, ledger.meters);
        assert_eq!(funded.limits, ledger.limits);
        assert_eq!(apply(&funded, &grants, 101).unwrap(), funded);
        assert!(apply(&funded, &BTreeMap::new(), 101).is_err());
        assert!(apply(&funded, &grants, 500).is_err());
        let mut replaced = grants.clone();
        replaced.get_mut(&grant.id).unwrap().root.resource.uid = "replacement".into();
        assert!(apply(&funded, &replaced, 101).is_err());
    }

    #[test]
    fn renewal_covers_only_exact_applied_expired_authority_and_revocation_still_blocks() {
        let (mut ledger, mut old, _) = fixture();
        ledger.root.kind = RootKind::KarsSandbox;
        ledger.root.resource.name = "sre".into();
        ledger.root.resource.uid = "principal-uid".into();
        let original = crate::inference_budget_contract::BuiltinFunding {
            root: ledger.root.clone(),
            limits: ledger.limits,
            expires_at: "1970-01-01T00:08:20Z".into(),
            unbounded_tokens: false,
        };
        let authority = &mut ledger.nodes.get_mut("principal-uid").unwrap().authority;
        authority.task = ledger.root.resource.clone();
        authority.effective_authorization = serde_json::to_value(&original).unwrap();
        old.root = ledger.root.clone();
        old.expires_at = original.expires_at.clone();
        let before = ledger.apply_token_top_up(&old, 100).unwrap().next;
        let mut renewal = old.clone();
        renewal.id = "renewal".into();
        renewal.expires_at = "1970-01-01T00:16:40Z".into();
        renewal.funding_renewal = Some(crate::inference_budget_contract::ledger::FundingRenewal {
            bridge_reactivation: None,
            registration_uid: "registration-uid".into(),
            intent_uid: "intent-uid".into(),
            intent_digest: "a".repeat(64),
            original_expires_at: original.expires_at.clone(),
            previous_top_up_ids: vec![old.id.clone()],
        });
        let mut grants = BTreeMap::from([
            (old.id.clone(), old.clone()),
            (renewal.id.clone(), renewal.clone()),
        ]);
        let after = apply(&before, &grants, 500).unwrap();
        assert_eq!(after.meters, before.meters);
        assert_eq!(after.nodes, before.nodes);
        assert_eq!(apply(&after, &grants, 501).unwrap(), after);
        assert!(apply(&after, &grants, 1000).is_err());
        grants.remove(&old.id);
        assert!(apply(&before, &grants, 500).is_err());
        assert!(apply(&after, &grants, 501).is_err());
        grants.insert(old.id.clone(), old.clone());
        grants.get_mut(&old.id).unwrap().tokens += 1;
        assert!(apply(&after, &grants, 501).is_err());
        grants.insert(old.id.clone(), old.clone());
        let mut unapplied = old;
        unapplied.id = "unapplied".into();
        grants.insert(unapplied.id.clone(), unapplied);
        assert!(apply(&before, &grants, 500).is_err());
    }

    #[test]
    fn bridge_reactivation_applies_through_existing_receipts_and_fails_closed_on_races_or_revocation()
     {
        let value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../shared/inference_budget/bridge-renewal-fixture.json"
        ))
        .unwrap();
        let before: Ledger = serde_json::from_value(value["ledger"].clone()).unwrap();
        let renewal: TokenTopUp = serde_json::from_value(value["grant"].clone()).unwrap();
        let mut grants = before.token_top_ups.clone();
        grants.insert(renewal.id.clone(), renewal.clone());
        let now = 1_792_454_400;
        let applied = apply(&before, &grants, now).unwrap();
        assert_eq!(
            applied,
            before.apply_token_top_up(&renewal, now).unwrap().next
        );
        assert_eq!(apply(&applied, &grants, now).unwrap(), applied);
        assert!(apply(&applied.close_account().unwrap().next, &grants, now).is_err());
        let mut raced = before.clone();
        raced.meters.settled.tokens += 1;
        raced
            .nodes
            .get_mut("sandbox-uid")
            .unwrap()
            .meters
            .settled
            .tokens += 1;
        raced.validate().unwrap();
        assert!(apply(&raced, &grants, now).is_err());
        grants.remove("topup-1");
        assert!(apply(&before, &grants, now).is_err());
        assert!(validate_receipts(&applied, &grants, now).is_err());
        grants.insert("topup-1".into(), before.token_top_ups["topup-1"].clone());
        grants.get_mut("topup-1").unwrap().tokens += 1;
        assert!(apply(&before, &grants, now).is_err());
        assert!(validate_receipts(&applied, &grants, now).is_err());
    }

    #[test]
    fn duplicate_malformed_and_foreign_authorizations_are_rejected() {
        let (ledger, mut grant, mut map) = fixture();
        map.data.as_mut().unwrap().insert(
            CONFIG_KEY.into(),
            serde_json::to_string(&vec![&grant, &grant]).unwrap(),
        );
        assert!(configured(&map).is_err());
        map.data
            .as_mut()
            .unwrap()
            .insert(CONFIG_KEY.into(), "{}".into());
        assert!(configured(&map).is_err());
        grant.root.cluster_uid = "foreign-cluster".into();
        assert!(apply(&ledger, &BTreeMap::from([(grant.id.clone(), grant)]), 100).is_err());
    }
}
