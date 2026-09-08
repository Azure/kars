// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use k8s_openapi::api::core::v1::Namespace;
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
};
use serde_json::json;
use std::collections::BTreeSet;

use super::{
    account::{KarsBudgetAccountSpec, name_for_root},
    config::Settings,
    store::{Store, StoreError},
};
use crate::{
    inference_budget_contract::{
        AccountReference, BudgetError, BudgetScope, Limits, ResourceIdentity, RootIdentity,
        RootKind, TaskAuthority, TaskBudgetBinding,
    },
    kars_task::{KarsTask, TaskEnvelope},
    kars_team::KarsTeam,
};

fn api_error(stage: &'static str, error: kube::Error) -> StoreError {
    StoreError::Api {
        stage,
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

pub fn limits(envelope: &TaskEnvelope) -> Result<Limits, StoreError> {
    let Some(budget) = &envelope.budget else {
        return Ok(Limits::default());
    };
    let convert = |value: Option<i64>| -> Result<Option<u64>, StoreError> {
        value
            .map(|value| {
                u64::try_from(value).map_err(|_| StoreError::Ledger(BudgetError::Authorization))
            })
            .transpose()
            .map(|value| value.filter(|value| *value > 0))
    };
    let limits = Limits {
        tokens: convert(budget.tokens)?,
        usd_micros: convert(budget.usd_micros)?,
    };
    if limits.finite() && budget.scope != Some(BudgetScope::GovernedInference) {
        return Err(BudgetError::Authorization.into());
    }
    Ok(limits)
}

pub fn has_finite(envelope: &TaskEnvelope) -> bool {
    envelope.budget.as_ref().is_some_and(|budget| {
        budget.tokens.is_some_and(|value| value > 0)
            || budget.usd_micros.is_some_and(|value| value > 0)
    })
}

fn resource(task: &KarsTask) -> Result<ResourceIdentity, StoreError> {
    let resource = ResourceIdentity {
        namespace: task.namespace().ok_or(BudgetError::Identity)?,
        name: task.name_any(),
        uid: task.uid().ok_or(BudgetError::Identity)?,
    };
    resource.validate()?;
    Ok(resource)
}

async fn chain(client: &Client, task: &KarsTask) -> Result<Vec<KarsTask>, StoreError> {
    let namespace = task.namespace().ok_or(BudgetError::Identity)?;
    let api: Api<KarsTask> = Api::namespaced(client.clone(), &namespace);
    let mut nodes = Vec::new();
    let mut current = task.clone();
    let mut seen = BTreeSet::new();
    loop {
        let id = resource(&current)?;
        if !seen.insert(id.uid)
            || nodes.len() >= crate::inference_budget_contract::MAX_NODES
            || current.metadata.deletion_timestamp.is_some()
        {
            return Err(BudgetError::Authorization.into());
        }
        limits(&current.spec.envelope)?;
        let parent = current
            .spec
            .parent_ref
            .as_ref()
            .map(|reference| reference.name.clone());
        nodes.push(current.clone());
        let Some(name) = parent else { break };
        let parent = api
            .get(&name)
            .await
            .map_err(|e| api_error("resolve budget parent UID", e))?;
        if current
            .status
            .as_ref()
            .and_then(|status| status.inference_budget.as_ref())
            .and_then(|binding| binding.parent_task_uid.as_deref())
            .is_some_and(|uid| parent.metadata.uid.as_deref() != Some(uid))
            || !crate::kars_task::spec_attenuation_violations(&current.spec, &parent.spec)
                .is_empty()
        {
            return Err(BudgetError::Authorization.into());
        }
        current = parent;
    }
    nodes.reverse();
    Ok(nodes)
}

pub(super) async fn needs_account(client: &Client, task: &KarsTask) -> Result<bool, StoreError> {
    let nodes = chain(client, task).await?;
    if nodes.iter().any(|task| {
        has_finite(&task.spec.envelope)
            || task
                .status
                .as_ref()
                .is_some_and(|status| status.inference_budget.is_some())
    }) {
        return Ok(true);
    }
    let first = nodes.first().ok_or(BudgetError::Identity)?;
    Ok(team_for_root(client, first).await?.is_some_and(|team| {
        has_finite(&team.spec.envelope)
            || team
                .status
                .as_ref()
                .is_some_and(|status| status.inference_budget_account.is_some())
    }))
}

pub async fn close_task(client: &Client, task: &KarsTask) -> Result<(), StoreError> {
    let Some(binding) = task
        .status
        .as_ref()
        .and_then(|status| status.inference_budget.as_ref())
    else {
        return Ok(());
    };
    if task.metadata.uid.as_deref() != Some(binding.task_uid.as_str()) {
        return Err(BudgetError::Identity.into());
    }
    let store = Store::new(client.clone(), &binding.account.namespace);
    // A crash may occur after the protected UID pin but before bootstrap
    // initialization/task registration. Seal that exact pending anchor only;
    // initialize rejects missing/replaced/sealed-corrupt prior accounting.
    store
        .initialize(&binding.root, &binding.account.uid)
        .await?;
    store
        .transact(&binding.root, &binding.account.uid, |ledger| {
            if binding.root.kind == RootKind::KarsTask
                && binding.root.resource.uid == binding.task_uid
            {
                ledger.close_account()
            } else {
                ledger.close_registered_task(&binding.task_uid)
            }
        })
        .await
}

pub async fn prepare_task(client: &Client, task: &KarsTask) -> Result<KarsTask, StoreError> {
    if !has_finite(&task.spec.envelope)
        && task
            .status
            .as_ref()
            .is_none_or(|status| status.inference_budget.is_none())
    {
        return Ok(task.clone());
    }
    if !needs_account(client, task).await? {
        return Ok(task.clone());
    }
    let settings = Settings::from_env()?.ok_or(BudgetError::Contract)?;
    let binding = ensure_task(client, task, &settings).await?;
    let api: Api<KarsTask> = Api::namespaced(
        client.clone(),
        &task.namespace().ok_or(BudgetError::Identity)?,
    );
    let live = api
        .get(&task.name_any())
        .await
        .map_err(|error| api_error("read prepared task", error))?;
    if live.metadata.uid != task.metadata.uid
        || live.metadata.generation != task.metadata.generation
        || live
            .status
            .as_ref()
            .and_then(|status| status.inference_budget.as_ref())
            != Some(&binding)
    {
        return Err(BudgetError::Identity.into());
    }
    Ok(live)
}

fn task_binding(
    task: &KarsTask,
    parent: Option<&KarsTask>,
    root: &RootIdentity,
    root_task_uid: &str,
    account: &AccountReference,
) -> Result<TaskBudgetBinding, StoreError> {
    Ok(TaskBudgetBinding {
        scope: BudgetScope::GovernedInference,
        account: account.clone(),
        root: root.clone(),
        task_uid: task.uid().ok_or(BudgetError::Identity)?,
        parent_task_uid: parent.and_then(ResourceExt::uid),
        root_task_uid: root_task_uid.into(),
        authorization_digest: task.envelope_digest(),
    })
}

async fn pin_task(
    client: &Client,
    task: &KarsTask,
    binding: &TaskBudgetBinding,
) -> Result<KarsTask, StoreError> {
    let api: Api<KarsTask> = Api::namespaced(
        client.clone(),
        &task.namespace().ok_or(BudgetError::Identity)?,
    );
    let live = api
        .get(&task.name_any())
        .await
        .map_err(|e| api_error("read task budget binding", e))?;
    if live.metadata.uid != task.metadata.uid
        || live.envelope_digest() != task.envelope_digest()
        || live.metadata.deletion_timestamp.is_some()
    {
        return Err(BudgetError::Identity.into());
    }
    if let Some(old) = live
        .status
        .as_ref()
        .and_then(|status| status.inference_budget.as_ref())
    {
        if old.account != binding.account
            || old.root != binding.root
            || old.task_uid != binding.task_uid
            || old.parent_task_uid != binding.parent_task_uid
            || old.root_task_uid != binding.root_task_uid
        {
            return Err(BudgetError::Identity.into());
        }
        if old == binding {
            return Ok(live);
        }
    }
    let uid = live.uid().ok_or(BudgetError::Identity)?;
    let rv = live.resource_version().ok_or(BudgetError::Identity)?;
    let updated = api.patch_status(&live.name_any(), &PatchParams::default(), &Patch::Merge(json!({
        "metadata": {"uid": uid, "resourceVersion": rv}, "status": {"inferenceBudget": binding}
    }))).await.map_err(|e| api_error("pin task inference account", e))?;
    if updated
        .status
        .as_ref()
        .and_then(|status| status.inference_budget.as_ref())
        != Some(binding)
    {
        return Err(StoreError::Missing);
    }
    Ok(updated)
}

async fn team_for_root(client: &Client, task: &KarsTask) -> Result<Option<KarsTeam>, StoreError> {
    let owners = task
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default();
    let Some(owner) = owners.iter().find(|owner| {
        owner.controller == Some(true)
            && owner.kind == "KarsTeam"
            && owner.api_version == "kars.azure.com/v1alpha1"
    }) else {
        return Ok(None);
    };
    let api: Api<KarsTeam> = Api::namespaced(
        client.clone(),
        &task.namespace().ok_or(BudgetError::Identity)?,
    );
    let team = api
        .get(&owner.name)
        .await
        .map_err(|e| api_error("resolve lifetime Team UID", e))?;
    if team.metadata.uid.as_deref() != Some(owner.uid.as_str())
        || team.metadata.deletion_timestamp.is_some()
    {
        return Err(BudgetError::Identity.into());
    }
    Ok(Some(team))
}

/// Bind a complete immutable UID ancestry and current full authorization before
/// execution. A missing/corrupt pinned account is never recreated as a balance
/// of zero. This routine does not relax any launch gate by itself.
pub async fn ensure_task(
    client: &Client,
    task: &KarsTask,
    settings: &Settings,
) -> Result<TaskBudgetBinding, StoreError> {
    settings
        .catalog(client, chrono::Utc::now().timestamp())
        .await?;
    let nodes = chain(client, task).await?;
    let first = nodes.first().ok_or(BudgetError::Identity)?;
    let first_id = resource(first)?;
    let team = team_for_root(client, first).await?;
    let namespaces: Api<Namespace> = Api::all(client.clone());
    let workspace = namespaces
        .get(&first_id.namespace)
        .await
        .map_err(|e| api_error("resolve budget workspace UID", e))?;
    let cluster = namespaces
        .get("kube-system")
        .await
        .map_err(|e| api_error("resolve budget cluster UID", e))?;
    let root = RootIdentity {
        kind: if team.is_some() {
            RootKind::KarsTeam
        } else {
            RootKind::KarsTask
        },
        resource: match &team {
            Some(team) => ResourceIdentity {
                namespace: first_id.namespace.clone(),
                name: team.name_any(),
                uid: team.uid().ok_or(BudgetError::Identity)?,
            },
            None => first_id.clone(),
        },
        workspace_uid: workspace.uid().ok_or(BudgetError::Identity)?,
        cluster_uid: cluster.uid().ok_or(BudgetError::Identity)?,
    };
    root.validate()?;
    let root_limits = limits(
        team.as_ref()
            .map(|team| &team.spec.envelope)
            .unwrap_or(&first.spec.envelope),
    )?;
    let store = Store::new(client.clone(), &settings.accounting_namespace);
    let existing = match &team {
        Some(team) => team
            .status
            .as_ref()
            .and_then(|status| status.inference_budget_account.clone()),
        None => first
            .status
            .as_ref()
            .and_then(|status| status.inference_budget.as_ref())
            .map(|binding| binding.account.clone()),
    };
    let account = if let Some(reference) = existing {
        if reference.namespace != settings.accounting_namespace
            || reference.name != name_for_root(&root)
        {
            return Err(BudgetError::Identity.into());
        }
        reference
    } else {
        crate::sre_authority::privacy_epoch(client, &settings.accounting_namespace)
            .await
            .map_err(|_| BudgetError::Authorization)?;
        let signer = crate::providers::signing::load_existing(client)
            .await
            .map_err(|_| BudgetError::Authorization)?;
        let anchor = store
            .create_anchor(
                KarsBudgetAccountSpec {
                    scope: BudgetScope::GovernedInference,
                    root: root.clone(),
                    limits: root_limits,
                },
                &signer,
            )
            .await?;
        let reference = AccountReference {
            namespace: settings.accounting_namespace.clone(),
            name: anchor.name_any(),
            uid: anchor.uid().ok_or(BudgetError::Identity)?,
        };
        if let Some(team) = &team {
            let api: Api<KarsTeam> = Api::namespaced(client.clone(), &root.resource.namespace);
            let updated = api
                .patch_status(
                    &team.name_any(),
                    &PatchParams::default(),
                    &Patch::Merge(json!({
                        "metadata": {"uid": team.uid(), "resourceVersion": team.resource_version()},
                        "status": {"inferenceBudgetAccount": reference}
                    })),
                )
                .await
                .map_err(|e| api_error("pin lifetime Team account", e))?;
            if updated
                .status
                .as_ref()
                .and_then(|status| status.inference_budget_account.as_ref())
                != Some(&reference)
            {
                return Err(StoreError::Missing);
            }
        } else {
            pin_task(
                client,
                first,
                &task_binding(first, None, &root, &first_id.uid, &reference)?,
            )
            .await?;
        }
        reference
    };
    let initialized = store.initialize(&root, &account.uid).await?;
    if initialized
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .is_none_or(|ledger| ledger.phase != crate::inference_budget_contract::AccountPhase::Active)
    {
        return Err(BudgetError::Closed.into());
    }
    let current_limits = initialized
        .status
        .as_ref()
        .and_then(|status| status.ledger.as_ref())
        .ok_or(StoreError::Missing)?
        .limits;
    if current_limits != root_limits {
        store
            .transact(&root, &account.uid, |ledger| {
                ledger.narrow_root_limits(root_limits)
            })
            .await?;
    }
    let default_model = crate::kars_task::blueprint::controller_default_model();
    let mut output = None;
    for (index, node) in nodes.iter().enumerate() {
        let authority = TaskAuthority {
            task: resource(node)?,
            parent_uid: index.checked_sub(1).and_then(|index| nodes[index].uid()),
            root_task_uid: first_id.uid.clone(),
            authorization_digest: node.spec.authorization_digest_with_model(&default_model),
            effective_authorization: node
                .spec
                .authorization_configuration_with_model(&default_model),
            limits: limits(&node.spec.envelope)?,
        };
        let current = store.read(&root, &account.uid).await?;
        let old = current
            .status
            .as_ref()
            .and_then(|status| status.ledger.as_ref())
            .and_then(|ledger| ledger.nodes.get(&authority.task.uid));
        match old {
            Some(old) if old.authority != authority => {
                store
                    .transact(&root, &account.uid, |ledger| {
                        ledger.close_subtree(&authority.task.uid)
                    })
                    .await?;
                store
                    .transact(&root, &account.uid, |ledger| {
                        ledger.update_authority(authority.clone())
                    })
                    .await?;
                store
                    .transact(&root, &account.uid, |ledger| {
                        ledger.resume_task(&authority.task.uid, &authority.authorization_digest)
                    })
                    .await?;
            }
            Some(old) if !old.active => {
                store
                    .transact(&root, &account.uid, |ledger| {
                        ledger.resume_task(&authority.task.uid, &authority.authorization_digest)
                    })
                    .await?;
            }
            Some(_) => {}
            None => {
                store
                    .transact(&root, &account.uid, |ledger| {
                        ledger.register_task(authority.clone())
                    })
                    .await?;
            }
        }
        let binding = task_binding(
            node,
            index.checked_sub(1).map(|index| &nodes[index]),
            &root,
            &first_id.uid,
            &account,
        )?;
        pin_task(client, node, &binding).await?;
        output = Some(binding);
    }
    output.ok_or_else(|| BudgetError::Identity.into())
}
