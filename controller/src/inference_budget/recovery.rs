// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    account::{BOOTSTRAP, KarsBudgetAccount, MANAGED_BY, OWNER},
    config::Settings,
    store::{Store, StoreError},
};
use crate::{
    inference_budget_contract::{BudgetError, RootKind, ledger::Mutation},
    kars_task::KarsTask,
    kars_team::KarsTeam,
};
use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::{Api, Client, ResourceExt, api::ListParams};

fn api_error(error: kube::Error) -> StoreError {
    StoreError::Api {
        stage: "reconcile inference liabilities",
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

pub fn start(client: Client, settings: Settings) {
    tokio::spawn(async move {
        loop {
            if scan(&client, &settings).await.is_err() {
                tracing::error!(
                    "Governed inference recovery unavailable; outstanding liabilities remain funded"
                );
            }
            tokio::time::sleep(std::time::Duration::from_secs(15)).await;
        }
    });
}

async fn scan(client: &Client, settings: &Settings) -> Result<(), StoreError> {
    let api: Api<KarsBudgetAccount> =
        Api::namespaced(client.clone(), &settings.accounting_namespace);
    let mut params = ListParams::default()
        .labels(&format!("{MANAGED_BY}={OWNER}"))
        .limit(50);
    let store = Store::new(client.clone(), &settings.accounting_namespace);
    loop {
        let page = api.list(&params).await.map_err(api_error)?;
        for account in page.items {
            if reconcile_account(client, &store, &account).await.is_err() {
                tracing::error!(account = %account.name_any(),
                    "Governed inference account recovery failed closed; no balances reset");
            }
        }
        let Some(token) = page.metadata.continue_.filter(|token| !token.is_empty()) else {
            break;
        };
        params = params.continue_token(&token);
    }
    Ok(())
}

pub(super) async fn reconcile_account(
    client: &Client,
    store: &Store,
    account: &KarsBudgetAccount,
) -> Result<(), StoreError> {
    let result = if account.annotations().get(BOOTSTRAP).map(String::as_str) == Some("pending") {
        Ok(())
    } else {
        recover(client, store, account).await
    };
    store
        .refresh_status(
            &account.spec.root,
            account.metadata.uid.as_deref().ok_or(StoreError::Missing)?,
            result.as_ref().err(),
        )
        .await?;
    result
}

async fn recover(
    client: &Client,
    store: &Store,
    account: &KarsBudgetAccount,
) -> Result<(), StoreError> {
    let root = &account.spec.root;
    let uid = account.uid().ok_or(BudgetError::Identity)?;
    let now = chrono::Utc::now().timestamp();
    store
        .transact(root, &uid, |ledger| ledger.expire_undispatched(now))
        .await?;
    // Provider transport deadline is 600s. A vanished router's liability is
    // committed after that deadline plus a margin, not refunded on TTL.
    store
        .transact(root, &uid, |ledger| {
            ledger.commit_uncertain_before(now - 660)
        })
        .await?;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let workspace = namespaces
        .get_opt(&root.resource.namespace)
        .await
        .map_err(api_error)?;
    if workspace.is_none_or(|namespace| {
        namespace.metadata.uid.as_deref() != Some(root.workspace_uid.as_str())
            || namespace.metadata.deletion_timestamp.is_some()
    }) {
        return store
            .transact(root, &uid, |ledger| ledger.close_account())
            .await;
    }
    let tasks: Api<KarsTask> = Api::namespaced(client.clone(), &root.resource.namespace);
    if root.kind == RootKind::KarsTeam {
        let teams: Api<KarsTeam> = Api::namespaced(client.clone(), &root.resource.namespace);
        let team = teams
            .get_opt(&root.resource.name)
            .await
            .map_err(api_error)?;
        if team.as_ref().is_none_or(|team| {
            team.metadata.uid.as_deref() != Some(root.resource.uid.as_str())
                || team.metadata.deletion_timestamp.is_some()
        }) {
            return store
                .transact(root, &uid, |ledger| ledger.close_account())
                .await;
        }
        if team.is_some_and(|team| team.spec.paused) {
            let current = store.read(root, &uid).await?;
            let ledger = current
                .status
                .as_ref()
                .and_then(|status| status.ledger.as_ref())
                .ok_or(StoreError::Missing)?;
            for node in ledger
                .nodes
                .values()
                .filter(|node| node.authority.parent_uid.is_none())
            {
                store
                    .transact(root, &uid, |ledger| {
                        ledger.close_subtree(&node.authority.task.uid)
                    })
                    .await?;
            }
            return Ok(());
        }
    } else {
        let task = tasks
            .get_opt(&root.resource.name)
            .await
            .map_err(api_error)?;
        if task.is_none_or(|task| {
            task.metadata.uid.as_deref() != Some(root.resource.uid.as_str())
                || task.metadata.deletion_timestamp.is_some()
        }) {
            return store
                .transact(root, &uid, |ledger| ledger.close_account())
                .await;
        }
    }
    let current = store.read(root, &uid).await?;
    let ledger = current
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .ok_or(StoreError::Missing)?;
    for node in ledger.nodes.values().filter(|node| node.active) {
        let task = tasks
            .get_opt(&node.authority.task.name)
            .await
            .map_err(api_error)?;
        if task.as_ref().is_none_or(|task| {
            task.metadata.uid.as_deref() != Some(node.authority.task.uid.as_str())
                || task.metadata.deletion_timestamp.is_some()
                || task.envelope_digest() != node.authority.authorization_digest
        }) {
            store
                .transact(root, &uid, |ledger| {
                    let current = ledger
                        .nodes
                        .get(&node.authority.task.uid)
                        .ok_or(BudgetError::Identity)?;
                    if current.authority != node.authority {
                        // A newer enrollment may already have funded sessions.
                        // Defer its live-source check to the next recovery scan,
                        // including when the authority changed during CAS retry.
                        return Ok(Mutation {
                            next: ledger.clone(),
                            value: (),
                            changed: false,
                        });
                    }
                    ledger.close_subtree(&node.authority.task.uid)
                })
                .await?;
            continue;
        }
        let launched = task.is_some_and(|task| {
            task.spec
                .execution
                .is_some_and(|execution| execution.launch)
        });
        for session in ledger.sessions.values().filter(|session| {
            !session.closed && session.identity.task_uid == node.authority.task.uid
        }) {
            let namespace = format!("kars-{}", session.identity.sandbox.name);
            let pods: Api<Pod> = Api::namespaced(client.clone(), &namespace);
            let pod = pods
                .get_opt(&session.identity.pod_name)
                .await
                .map_err(api_error)?;
            if !launched
                || pod.is_none_or(|pod| {
                    pod.metadata.uid.as_deref() != Some(session.identity.pod_uid.as_str())
                        || pod.metadata.deletion_timestamp.is_some()
                })
            {
                store
                    .transact(root, &uid, |ledger| {
                        ledger.close_session(&session.identity.pod_uid)
                    })
                    .await?;
            }
        }
    }
    Ok(())
}
