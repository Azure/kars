// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Durable single-object accounting. No informer balance and no in-memory
//! authority. A grant is returned only after its UID/RV-fenced write succeeds.

use super::account::*;
use crate::inference_budget_contract::{
    BudgetError, RootIdentity,
    ledger::{Ledger, Mutation},
};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::json;

const RETRIES: usize = 8;
const MAX_OPERATION_SECONDS: u64 = 10;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Ledger(#[from] BudgetError),
    #[error("Inference budget API {stage} failed (status {code:?})")]
    Api {
        stage: &'static str,
        code: Option<u16>,
    },
    #[error("Inference budget account is missing, replaced, or uninitialized")]
    Missing,
    #[error("Inference budget account CAS contention/deadline exceeded")]
    Contention,
}

impl From<crate::task_identity::Error> for StoreError {
    fn from(error: crate::task_identity::Error) -> Self {
        match error {
            crate::task_identity::Error::Api { stage, code } => Self::Api { stage, code },
            crate::task_identity::Error::Changed => Self::Contention,
            _ => Self::Ledger(BudgetError::Authorization),
        }
    }
}

fn api_error(stage: &'static str, error: kube::Error) -> StoreError {
    StoreError::Api {
        stage,
        code: match error {
            kube::Error::Api(status) => Some(status.code),
            _ => None,
        },
    }
}

#[derive(Clone)]
pub struct Store {
    accounts: Api<KarsBudgetAccount>,
    namespace: String,
}

impl Store {
    pub fn new(client: Client, accounting_namespace: &str) -> Self {
        Self {
            accounts: Api::namespaced(client, accounting_namespace),
            namespace: accounting_namespace.into(),
        }
    }

    fn validate_identity(
        account: &KarsBudgetAccount,
        root: &RootIdentity,
        uid: &str,
    ) -> Result<(), StoreError> {
        if account.metadata.uid.as_deref() != Some(uid)
            || account
                .metadata
                .resource_version
                .as_deref()
                .is_none_or(str::is_empty)
            || account.metadata.name.as_deref() != Some(name_for_root(root).as_str())
            || account.metadata.deletion_timestamp.is_some()
            || account.spec.root != *root
            || account.labels().get(MANAGED_BY).map(String::as_str) != Some(OWNER)
            || account
                .metadata
                .owner_references
                .as_ref()
                .is_some_and(|refs| !refs.is_empty())
        {
            return Err(StoreError::Missing);
        }
        Ok(())
    }

    pub(super) fn ledger(account: &KarsBudgetAccount) -> Result<&Ledger, StoreError> {
        let ledger = account
            .status
            .as_ref()
            .and_then(|status| status.ledger.as_ref())
            .ok_or(StoreError::Missing)?;
        if account.annotations().get(BOOTSTRAP).map(String::as_str) != Some("sealed")
            || account.metadata.uid.as_deref() != Some(ledger.account_uid.as_str())
            || account.spec.root != ledger.root
            || account.spec.scope != ledger.scope
            || !ledger.limits.attenuates(account.spec.limits)
        {
            return Err(StoreError::Missing);
        }
        ledger.validate()?;
        for node in ledger.nodes.values() {
            let mut effective = node.authority.effective_authorization.clone();
            effective.sort_all_objects();
            let bytes = serde_json::to_vec(&effective).map_err(|_| BudgetError::Corrupt)?;
            let digest = format!("sha256:{}", crate::providers::signing::sha256_hex(&bytes));
            if digest != node.authority.authorization_digest {
                return Err(BudgetError::Corrupt.into());
            }
        }
        Ok(ledger)
    }

    pub(super) fn bootstrap_ledger(account: &KarsBudgetAccount) -> Result<(), StoreError> {
        if let Some(ledger) = account
            .status
            .as_ref()
            .and_then(|status| status.ledger.as_ref())
        {
            ledger.validate()?;
            if account.metadata.uid.as_deref() != Some(ledger.account_uid.as_str())
                || ledger.root != account.spec.root
                || ledger.scope != account.spec.scope
                || ledger.limits.normalized() != account.spec.limits.normalized()
                || !ledger.nodes.is_empty()
                || !ledger.sessions.is_empty()
                || !ledger.attempts.is_empty()
                || ledger.meters != Default::default()
                || ledger.phase != crate::inference_budget_contract::AccountPhase::Active
            {
                return Err(BudgetError::Corrupt.into());
            }
        }
        Ok(())
    }

    /// Reserve an account anchor. The owning root's protected status must pin
    /// the returned UID BEFORE `initialize` is called. Existing callers with a
    /// pin use `read`, never this bootstrap path.
    pub async fn create_anchor(
        &self,
        spec: KarsBudgetAccountSpec,
        signer: &crate::providers::signing::ReceiptSigner,
    ) -> Result<KarsBudgetAccount, StoreError> {
        spec.root.validate()?;
        let name = name_for_root(&spec.root);
        let mut account = KarsBudgetAccount::new(&name, spec);
        let authority = super::claim::issue(&account.spec, &self.namespace, signer)?;
        account.metadata.labels = Some(std::collections::BTreeMap::from([(
            MANAGED_BY.into(),
            OWNER.into(),
        )]));
        account.metadata.annotations = Some(std::collections::BTreeMap::from([
            (BOOTSTRAP.into(), "pending".into()),
            (super::claim::ANNOTATION.into(), authority),
        ]));
        let created = match self.accounts.create(&PostParams::default(), &account).await {
            Ok(created) => Ok(created),
            Err(kube::Error::Api(status)) if status.code == 409 => {
                let existing = self
                    .accounts
                    .get(&name)
                    .await
                    .map_err(|e| api_error("read account anchor", e))?;
                let uid = existing
                    .metadata
                    .uid
                    .as_deref()
                    .ok_or(StoreError::Missing)?;
                Self::validate_identity(&existing, &account.spec.root, uid)?;
                super::claim::verify(&existing, &self.namespace, signer)?;
                if existing.spec.limits.normalized() != account.spec.limits.normalized() {
                    return Err(BudgetError::Authorization.into());
                }
                if existing.annotations().get(BOOTSTRAP).map(String::as_str) == Some("sealed") {
                    Self::ledger(&existing)?;
                } else if existing.annotations().get(BOOTSTRAP).map(String::as_str)
                    != Some("pending")
                {
                    return Err(StoreError::Missing);
                }
                Ok(existing)
            }
            Err(error) => Err(api_error("create account anchor", error)),
        }?;
        self.refresh_status(
            &created.spec.root,
            created.metadata.uid.as_deref().ok_or(StoreError::Missing)?,
            None,
        )
        .await
    }

    pub async fn initialize(
        &self,
        root: &RootIdentity,
        pinned_uid: &str,
    ) -> Result<KarsBudgetAccount, StoreError> {
        root.validate()?;
        if !crate::inference_budget_contract::valid_uid(pinned_uid) {
            return Err(BudgetError::Identity.into());
        }
        let name = name_for_root(root);
        for _ in 0..RETRIES {
            let account = self
                .accounts
                .get(&name)
                .await
                .map_err(|e| api_error("read account bootstrap", e))?;
            Self::validate_identity(&account, root, pinned_uid)?;
            if account.annotations().get(BOOTSTRAP).map(String::as_str) == Some("sealed") {
                Self::ledger(&account)?;
                return self.refresh_status(root, pinned_uid, None).await;
            }
            if account.annotations().get(BOOTSTRAP).map(String::as_str) != Some("pending") {
                return Err(StoreError::Missing);
            }
            Self::bootstrap_ledger(&account)?;
            if account
                .status
                .as_ref()
                .and_then(|status| status.ledger.as_ref())
                .is_some()
            {
                // A crash after status initialization must validate the existing
                // ledger, not reset it. Pending accounts cannot dispatch.
                match self.accounts.patch(&name, &PatchParams::default(), &Patch::Merge(json!({
                    "metadata": {"uid": pinned_uid, "resourceVersion": account.resource_version(),
                        "annotations": {BOOTSTRAP: "sealed"}}
                }))).await {
                    Ok(sealed) => {
                        Self::ledger(&sealed)?;
                        return self.refresh_status(root, pinned_uid, None).await;
                    }
                    Err(kube::Error::Api(status)) if status.code == 409 => continue,
                    Err(error) => return Err(api_error("seal account bootstrap", error)),
                }
            }
            let ledger = Ledger::new(pinned_uid.into(), root.clone(), account.spec.limits)?;
            let mut next = account.clone();
            next.status.get_or_insert_with(Default::default).ledger = Some(ledger);
            next.status = Some(super::status::project(&next, None));
            match self
                .accounts
                .replace_status(&name, &PostParams::default(), &next)
                .await
            {
                Ok(_) => {}
                Err(kube::Error::Api(status)) if status.code == 409 => continue,
                Err(error) => return Err(api_error("initialize account ledger", error)),
            }
        }
        Err(StoreError::Contention)
    }

    pub async fn read(
        &self,
        root: &RootIdentity,
        account_uid: &str,
    ) -> Result<KarsBudgetAccount, StoreError> {
        root.validate()?;
        if !crate::inference_budget_contract::valid_uid(account_uid) {
            return Err(BudgetError::Identity.into());
        }
        let account = self
            .accounts
            .get(&name_for_root(root))
            .await
            .map_err(|e| api_error("read account", e))?;
        Self::validate_identity(&account, root, account_uid)?;
        Self::ledger(&account)?;
        Ok(account)
    }

    pub async fn transact<T>(
        &self,
        root: &RootIdentity,
        account_uid: &str,
        operation: impl Fn(&Ledger) -> Result<Mutation<T>, BudgetError>,
    ) -> Result<T, StoreError> {
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(MAX_OPERATION_SECONDS);
        for _ in 0..RETRIES {
            if tokio::time::Instant::now() >= deadline {
                return Err(StoreError::Contention);
            }
            let account = tokio::time::timeout_at(deadline, self.read(root, account_uid))
                .await
                .map_err(|_| StoreError::Contention)??;
            let mutation = operation(Self::ledger(&account)?)?;
            if !mutation.changed {
                return Ok(mutation.value);
            }
            mutation.next.validate()?;
            if mutation.next.account_uid != account_uid || mutation.next.root != *root {
                return Err(BudgetError::Identity.into());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(StoreError::Contention);
            }
            // Merge-patching maps would retain compacted attempt rows. Replace
            // the status as one value using PUT with UID/RV preconditions.
            let mut next = account.clone();
            next.status.get_or_insert_with(Default::default).ledger = Some(mutation.next);
            next.status = Some(super::status::project(&next, None));
            let committed = tokio::time::timeout_at(
                deadline,
                self.accounts
                    .replace_status(&name_for_root(root), &PostParams::default(), &next),
            )
            .await
            .map_err(|_| StoreError::Contention)?;
            match committed {
                Ok(stored) => {
                    Self::validate_identity(&stored, root, account_uid)?;
                    if Some(Self::ledger(&stored)?)
                        != next
                            .status
                            .as_ref()
                            .and_then(|status| status.ledger.as_ref())
                    {
                        return Err(BudgetError::Corrupt.into());
                    }
                    return Ok(mutation.value);
                }
                Err(kube::Error::Api(status)) if status.code == 409 => continue,
                Err(error) => return Err(api_error("commit ledger transition", error)),
            }
        }
        Err(StoreError::Contention)
    }

    /// Report only from a fresh UID/RV snapshot. Even corrupt or unavailable
    /// accounts keep their exact ledger; a report can never initialize funding.
    pub(super) async fn refresh_status(
        &self,
        root: &RootIdentity,
        account_uid: &str,
        error: Option<&StoreError>,
    ) -> Result<KarsBudgetAccount, StoreError> {
        root.validate()?;
        if !crate::inference_budget_contract::valid_uid(account_uid) {
            return Err(BudgetError::Identity.into());
        }
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(MAX_OPERATION_SECONDS);
        for _ in 0..RETRIES {
            let account =
                tokio::time::timeout_at(deadline, self.accounts.get(&name_for_root(root)))
                    .await
                    .map_err(|_| StoreError::Contention)?
                    .map_err(|error| api_error("read account observation", error))?;
            Self::validate_identity(&account, root, account_uid)?;
            let mut next = account.clone();
            next.status = Some(super::status::project(&account, error));
            if next.status == account.status {
                return Ok(account);
            }
            match tokio::time::timeout_at(
                deadline,
                self.accounts
                    .replace_status(&name_for_root(root), &PostParams::default(), &next),
            )
            .await
            .map_err(|_| StoreError::Contention)?
            {
                Ok(stored) => {
                    Self::validate_identity(&stored, root, account_uid)?;
                    if stored.status != next.status {
                        return Err(BudgetError::Corrupt.into());
                    }
                    return Ok(stored);
                }
                Err(kube::Error::Api(status)) if status.code == 409 => continue,
                Err(error) => return Err(api_error("commit account observation", error)),
            }
        }
        Err(StoreError::Contention)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
