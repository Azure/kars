// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    account::{KarsBudgetAccount, KarsBudgetAccountSpec},
    store::StoreError,
};
use crate::{inference_budget_contract::BudgetError, providers::signing::ReceiptSigner};
use kube::ResourceExt;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const ANNOTATION: &str = "kars.azure.com/inference-budget-authority";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Claim {
    key_id: String,
    nonce: String,
    signature: String,
}

fn note(spec: &KarsBudgetAccountSpec, namespace: &str, nonce: &str) -> Result<Vec<u8>, StoreError> {
    let mut value = json!({
        "domain":"kars.azure.com/inference-budget-bootstrap/v1",
        "accountingNamespace":namespace, "name":super::account::name_for_root(&spec.root),
        "grant":spec, "nonce":nonce,
    });
    value.sort_all_objects();
    serde_json::to_vec(&value).map_err(|_| BudgetError::Corrupt.into())
}

/// Authorship is signed atomically with CREATE, not inferred from labels on a
/// pre-existing object. Root status subsequently pins the API-generated UID.
pub fn issue(
    spec: &KarsBudgetAccountSpec,
    namespace: &str,
    signer: &ReceiptSigner,
) -> Result<String, StoreError> {
    let nonce = crate::providers::signing::generate_service_token();
    let signature = signer.sign_note(&note(spec, namespace, &nonce)?);
    serde_json::to_string(&Claim {
        key_id: signer.key_id.clone(),
        nonce,
        signature,
    })
    .map_err(|_| BudgetError::Corrupt.into())
}

pub fn verify(
    account: &KarsBudgetAccount,
    namespace: &str,
    signer: &ReceiptSigner,
) -> Result<(), StoreError> {
    let claim: Claim = serde_json::from_str(
        account
            .annotations()
            .get(ANNOTATION)
            .ok_or(BudgetError::Identity)?,
    )
    .map_err(|_| BudgetError::Identity)?;
    if claim.key_id != signer.key_id
        || claim.nonce.len() != 64
        || !signer.verify_note(
            &note(&account.spec, namespace, &claim.nonce)?,
            &claim.signature,
        )
    {
        return Err(BudgetError::Identity.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_budget_contract::{
        BudgetScope, Limits, ResourceIdentity, RootIdentity, RootKind,
    };

    #[test]
    fn forged_labels_or_a_copied_grant_from_another_workspace_do_not_prove_authorship() {
        let signer = ReceiptSigner::from_bytes(&[7; 32]);
        let spec = KarsBudgetAccountSpec {
            scope: BudgetScope::GovernedInference,
            limits: Limits::default(),
            root: RootIdentity {
                kind: RootKind::KarsTask,
                resource: ResourceIdentity {
                    namespace: "workspace".into(),
                    name: "root".into(),
                    uid: "root-uid".into(),
                },
                workspace_uid: "workspace-uid".into(),
                cluster_uid: "cluster-uid".into(),
            },
        };
        let mut account =
            KarsBudgetAccount::new(&super::super::account::name_for_root(&spec.root), spec);
        assert!(verify(&account, "accounting", &signer).is_err());
        account.metadata.annotations = Some(std::collections::BTreeMap::from([(
            ANNOTATION.into(),
            issue(&account.spec, "accounting", &signer).unwrap(),
        )]));
        assert!(verify(&account, "accounting", &signer).is_ok());
        assert!(verify(&account, "foreign-accounting", &signer).is_err());
        assert!(verify(&account, "accounting", &ReceiptSigner::from_bytes(&[8; 32])).is_err());
        account.spec.root.resource.uid = "recreated-root".into();
        assert!(verify(&account, "accounting", &signer).is_err());
    }
}
