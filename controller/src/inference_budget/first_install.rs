// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The registration resolves a Helm-owned intent to real UIDs. It is not a
//! second funding store: every use revalidates the original immutable intent.
use super::{config::Settings, store::StoreError};
use crate::{
    crd::KarsSandbox,
    inference_budget_contract::{BudgetError, BuiltinFunding},
    inference_policy::InferencePolicy,
    sre_registration::KarsSRERegistration,
    tool_policy::ToolPolicy,
};
use k8s_openapi::api::core::v1::{ConfigMap, Namespace};
use kube::{Api, Client, ResourceExt};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) const ANNOTATION: &str = "kars.azure.com/sre-first-install-intent";
const NAME: &str = "kars-sre-first-install";

#[path = "first_install_trust.rs"]
mod trust;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Intent {
    version: u32,
    release: String,
    namespace: String,
    values_digest: String,
    funding: Funding,
    resources: Vec<Value>,
    budget: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Funding {
    #[serde(default)]
    tokens: Option<u64>,
    #[serde(default)]
    expires_at: String,
    #[serde(default)]
    unbounded_tokens: bool,
}

fn owned(map: &ConfigMap, reg: &KarsSRERegistration) -> bool {
    map.metadata
        .uid
        .as_deref()
        .is_some_and(|uid| !uid.is_empty())
        && map.metadata.resource_version.is_some()
        && map.metadata.deletion_timestamp.is_none()
        && map.namespace().as_deref() == Some(reg.spec.controller.namespace.name.as_str())
        && map.annotations().get("meta.helm.sh/release-name") == Some(&reg.spec.controller.release)
        && map.annotations().get("meta.helm.sh/release-namespace")
            == Some(&reg.spec.controller.namespace.name)
}

fn intent(map: &ConfigMap, reg: &KarsSRERegistration, now: i64) -> Result<Intent, StoreError> {
    let bound = reg
        .spec
        .first_install_funding
        .as_ref()
        .ok_or(BudgetError::Authorization)?;
    let encoded = map
        .data
        .as_ref()
        .and_then(|data| data.get("intent.json"))
        .ok_or(BudgetError::Authorization)?;
    if encoded.len() > 131_072
        || !owned(map, reg)
        || map.immutable != Some(true)
        || map.name_any() != NAME
        || bound.intent.name != NAME
        || map.uid().as_deref() != Some(bound.intent.uid.as_str())
        || crate::providers::signing::sha256_hex(encoded.as_bytes()) != bound.intent.digest
    {
        return Err(BudgetError::Authorization.into());
    }
    let intent: Intent = serde_json::from_str(encoded).map_err(|_| BudgetError::Authorization)?;
    let grant = &bound.grant;
    grant.validate(now)?;
    if intent.version != 1
        || intent.release != reg.spec.controller.release
        || intent.namespace != reg.spec.controller.namespace.name
        || intent.values_digest.len() != 64
        || !intent
            .values_digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || intent
            .funding
            .tokens
            .is_some_and(|tokens| tokens == 0 || tokens > 9_007_199_254_740_991)
        || grant.limits.tokens != intent.funding.tokens
        || grant.limits.usd_micros.is_some()
        || grant.expires_at != intent.funding.expires_at
        || grant.unbounded_tokens != intent.funding.unbounded_tokens
        || grant.root.resource.uid != reg.spec.sandbox.uid
        || grant.root.resource.namespace != reg.spec.sandbox.namespace
        || grant.root.resource.name != "sre"
        || grant.root.workspace_uid != reg.spec.controller.namespace.uid
        || intent.resources.len() != 3
        || !reg.spec.legacy_bindings.is_empty()
        || reg.spec.legacy_consumer.is_some()
    {
        return Err(BudgetError::Authorization.into());
    }
    for (kind, name) in [
        ("KarsSandbox", "sre"),
        ("InferencePolicy", "sre-inference"),
        ("ToolPolicy", "sre-tools"),
    ] {
        if intent
            .resources
            .iter()
            .filter(|object| {
                object["apiVersion"] == "kars.azure.com/v1alpha1"
                    && object["kind"] == kind
                    && object["metadata"]["name"] == name
                    && object["metadata"]["namespace"] == intent.namespace
            })
            .count()
            != 1
        {
            return Err(BudgetError::Authorization.into());
        }
    }
    Ok(intent)
}

fn expected(intent: &Intent, kind: &str) -> Result<Value, StoreError> {
    intent
        .resources
        .iter()
        .find(|object| object["kind"] == kind)
        .cloned()
        .ok_or(BudgetError::Authorization.into())
}

fn validate_source_spec(source: &KarsSandbox, desired: Value) -> Result<(), StoreError> {
    let desired: KarsSandbox =
        serde_json::from_value(desired).map_err(|_| BudgetError::Authorization)?;
    let mut actual = source.spec.clone();
    actual.inference_budget_ref = None;
    if serde_json::to_value(&actual).map_err(|_| BudgetError::Authorization)?
        != serde_json::to_value(&desired.spec).map_err(|_| BudgetError::Authorization)?
    {
        return Err(BudgetError::Authorization.into());
    }
    Ok(())
}

fn verify_catalog_transition(
    current: &Value,
    original: &Value,
    now: i64,
) -> Result<(), BudgetError> {
    let mut comparison = current.clone();
    if current["nonInferenceEgressHosts"] != original["nonInferenceEgressHosts"] {
        let catalog: crate::inference_budget_contract::catalog::Catalog =
            serde_json::from_value(current.clone()).map_err(|_| BudgetError::Contract)?;
        catalog.validate(now)?;
        // Only the operator-owned catalog may declare exclusions. Sandbox allowlists
        // remain independently enforced and cannot bypass brokered model routes.
        comparison["nonInferenceEgressHosts"] = original["nonInferenceEgressHosts"].clone();
        if comparison == *original {
            return Ok(());
        }
    }
    verify_contract_renewal(&comparison, original, now)
}

fn verify_contract_renewal(current: &Value, original: &Value, now: i64) -> Result<(), BudgetError> {
    let mut comparison = current.clone();
    let version = current["version"].as_str().ok_or(BudgetError::Contract)?;
    if !crate::inference_budget_contract::valid_uid(version)
        || current["version"] == original["version"]
    {
        return Err(BudgetError::Authorization);
    }
    let selected = current["contracts"]
        .as_array()
        .ok_or(BudgetError::Contract)?;
    let sealed = original["contracts"]
        .as_array()
        .ok_or(BudgetError::Contract)?;
    if selected.is_empty() || selected.len() < sealed.len() {
        return Err(BudgetError::Authorization);
    }
    let added = selected.len() > sealed.len();
    if added {
        let catalog: crate::inference_budget_contract::catalog::Catalog =
            serde_json::from_value(current.clone()).map_err(|_| BudgetError::Contract)?;
        catalog.validate(now)?;
        let mut ids = std::collections::BTreeSet::new();
        let mut routes = std::collections::BTreeSet::new();
        for (index, contract) in catalog.contracts.iter().enumerate() {
            if !ids.insert(&contract.id)
                || !routes.insert((
                    &contract.provider_id,
                    &contract.model,
                    format!("{:?}", contract.operation),
                ))
            {
                return Err(BudgetError::Contract);
            }
            if index >= sealed.len() {
                // Replay proves catalog provenance, not permission to use every route.
                // Catalog::select checks the chosen contract's current expiry.
                contract.validate(i64::MIN)?;
                let expiry = chrono::DateTime::parse_from_rfc3339(&contract.valid_until)
                    .map_err(|_| BudgetError::Contract)?;
                if contract.valid_until.len() != 20
                    || !contract.valid_until.ends_with('Z')
                    || expiry.format("%Y-%m-%dT%H:%M:%SZ").to_string() != contract.valid_until
                    || sealed.iter().any(|baseline| {
                        baseline["providerId"] == contract.provider_id
                            && baseline["model"] == contract.model
                    })
                {
                    return Err(BudgetError::Authorization);
                }
            }
        }
    }
    let mut renewed = added;
    for (contract, baseline) in selected.iter().zip(sealed) {
        if contract == baseline {
            continue;
        }
        let version = contract["version"].as_str().ok_or(BudgetError::Contract)?;
        let expiry = contract["validUntil"]
            .as_str()
            .ok_or(BudgetError::Contract)?;
        let parsed =
            chrono::DateTime::parse_from_rfc3339(expiry).map_err(|_| BudgetError::Contract)?;
        let previous = chrono::DateTime::parse_from_rfc3339(
            baseline["validUntil"]
                .as_str()
                .ok_or(BudgetError::Contract)?,
        )
        .map_err(|_| BudgetError::Contract)?;
        let mut normalized = contract.as_object().ok_or(BudgetError::Contract)?.clone();
        normalized.insert("version".into(), baseline["version"].clone());
        normalized.insert("validUntil".into(), baseline["validUntil"].clone());
        if !crate::inference_budget_contract::valid_uid(version)
            || contract["version"] == baseline["version"]
            || expiry.len() != 20
            || !expiry.ends_with('Z')
            || parsed.format("%Y-%m-%dT%H:%M:%SZ").to_string() != expiry
            || parsed <= previous
            || Value::Object(normalized) != *baseline
        {
            return Err(BudgetError::Authorization);
        }
        renewed = true;
    }
    comparison["version"] = original["version"].clone();
    comparison["contracts"] = original["contracts"].clone();
    if !renewed || comparison != *original {
        return Err(BudgetError::Authorization);
    }
    Ok(())
}

fn verify_contract_adoption(map: &ConfigMap, reg: &KarsSRERegistration) -> Result<(), BudgetError> {
    let bound = reg
        .spec
        .first_install_funding
        .as_ref()
        .ok_or(BudgetError::Authorization)?;
    let data = map.data.as_ref().ok_or(BudgetError::Authorization)?;
    let digest = data.get("valuesDigest").ok_or(BudgetError::Authorization)?;
    if !owned(map, reg)
        || map.name_any() != "kars-sre-helm-adoption"
        || map.immutable != Some(true)
        || data.get("intentUID") != Some(&bound.intent.uid)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(BudgetError::Authorization);
    }
    Ok(())
}

fn verify_adopted_policy_update(
    current: &Value,
    original: &Value,
    unbounded_tokens: bool,
) -> Result<(), BudgetError> {
    let mut comparison = current.clone();
    // Only an already validated unbounded funding grant authorizes removing
    // calendar or request ceilings. It does not authorize other policy drift.
    if unbounded_tokens
        && let Some(original_budget) = original.get("tokenBudget").and_then(Value::as_object)
    {
        match comparison.get_mut("tokenBudget") {
            None | Some(Value::Null) => {
                comparison["tokenBudget"] = Value::Object(original_budget.clone());
            }
            Some(Value::Object(budget)) => {
                for field in ["dailyTokens", "monthlyTokens", "perRequestTokens"] {
                    if let Some(value) = original_budget.get(field) {
                        let selected = budget.entry(field).or_insert(Value::Null);
                        if selected.is_null() {
                            *selected = value.clone();
                        }
                    }
                }
            }
            _ => return Err(BudgetError::Authorization),
        }
    }
    let path = "/tokenBudget/dailyTokens";
    match (comparison.pointer(path), original.pointer(path)) {
        (None, None) | (Some(Value::Null), Some(Value::Null)) => {}
        (Some(selected), Some(sealed)) => {
            let selected = selected.as_u64().ok_or(BudgetError::Authorization)?;
            let sealed = sealed.as_u64().ok_or(BudgetError::Authorization)?;
            if selected != sealed
                && (sealed == 0 || selected < sealed || selected > 9_007_199_254_740_991)
            {
                return Err(BudgetError::Authorization);
            }
            *comparison
                .pointer_mut(path)
                .ok_or(BudgetError::Authorization)? = json!(sealed);
        }
        _ => return Err(BudgetError::Authorization),
    }
    let primary_path = "/modelPreference/primary";
    if current.pointer(primary_path) != original.pointer(primary_path) {
        let primary = current
            .pointer(primary_path)
            .and_then(Value::as_object)
            .ok_or(BudgetError::Authorization)?;
        let provider = primary
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("");
        let model = primary
            .get("deployment")
            .and_then(Value::as_str)
            .unwrap_or("");
        if primary.len() != 2
            || provider.is_empty()
            || provider.len() > 64
            || !provider.as_bytes()[0].is_ascii_lowercase()
            || !provider
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || model.is_empty()
            || model.chars().count() > 256
            || model.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(BudgetError::Authorization);
        }
        *comparison
            .pointer_mut(primary_path)
            .ok_or(BudgetError::Authorization)? = original
            .pointer(primary_path)
            .ok_or(BudgetError::Authorization)?
            .clone();
    }
    if comparison != *original {
        return Err(BudgetError::Authorization);
    }
    Ok(())
}

async fn require_adopted_transition(
    client: &Client,
    reg: &KarsSRERegistration,
    source: &KarsSandbox,
) -> Result<(), StoreError> {
    if source.spec.inference_budget_ref.is_none() {
        return Err(BudgetError::Authorization.into());
    }
    let adoption =
        Api::<ConfigMap>::namespaced(client.clone(), &reg.spec.controller.namespace.name)
            .get("kars-sre-helm-adoption")
            .await
            .map_err(|_| BudgetError::Authorization)?;
    verify_contract_adoption(&adoption, reg)?;
    Ok(())
}

pub(crate) async fn funding(
    client: &Client,
    settings: &Settings,
    source: &KarsSandbox,
    catalog: &ConfigMap,
) -> Result<BuiltinFunding, StoreError> {
    let reg = Api::<KarsSRERegistration>::all(client.clone())
        .get(crate::sre_registration::NAME)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    let grant = validate(client, settings, source, catalog, &reg).await?;
    authorize_funding(client, settings, source, &grant).await?;
    Ok(grant)
}

pub(super) async fn validate(
    client: &Client,
    settings: &Settings,
    source: &KarsSandbox,
    catalog: &ConfigMap,
    reg: &KarsSRERegistration,
) -> Result<BuiltinFunding, StoreError> {
    let now = chrono::Utc::now().timestamp();
    reg.validate().map_err(|_| BudgetError::Authorization)?;
    if !reg.spec.enabled
        || source.annotations().get(ANNOTATION).map(String::as_str) != Some(NAME)
        || source.uid().as_deref() != Some(reg.spec.sandbox.uid.as_str())
        || source.namespace().as_deref() != Some(reg.spec.sandbox.namespace.as_str())
        || source.metadata.deletion_timestamp.is_some()
        || !source.owner_references().is_empty()
        || source.annotations().get("meta.helm.sh/release-name")
            != Some(&reg.spec.controller.release)
        || source.annotations().get("meta.helm.sh/release-namespace")
            != Some(&reg.spec.controller.namespace.name)
        || settings.accounting_namespace != reg.spec.controller.namespace.name
        || !owned(catalog, reg)
    {
        return Err(BudgetError::Authorization.into());
    }
    let map = Api::<ConfigMap>::namespaced(client.clone(), &reg.spec.controller.namespace.name)
        .get(NAME)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    // The sealed expiry never changes. Current authority is checked separately
    // against the original grant or an exact, durable operator renewal.
    let intent = intent(&map, reg, i64::MIN)?;
    let data = catalog.data.as_ref().ok_or(BudgetError::Authorization)?;
    let contracts: Value = serde_json::from_str(
        data.get("contracts.json")
            .ok_or(BudgetError::Authorization)?,
    )
    .map_err(|_| BudgetError::Authorization)?;
    let grants: Vec<BuiltinFunding> = serde_json::from_str(
        data.get("built-in-funding.json")
            .ok_or(BudgetError::Authorization)?,
    )
    .map_err(|_| BudgetError::Authorization)?;
    if data.get("first-install-sre-intent").map(String::as_str) != Some(NAME)
        || grants.iter().any(|grant| grant.root.resource.name == "sre")
        || intent.budget["enabled"] != true
        || intent.budget["tlsSecretName"] != settings.tls_secret
        || !settings.matches_first_install_router(intent.budget["routerImageDigest"].as_str())
    {
        return Err(BudgetError::Authorization.into());
    }
    trust::validate_ca(
        client,
        settings,
        reg,
        source,
        intent.budget["caBundle"].as_str(),
        data.get("ca.crt").map(String::as_str),
    )
    .await?;
    let original_contracts = json!({"version":intent.budget["catalogVersion"], "contracts":intent.budget["contracts"],
        "nonInferenceEgressHosts":intent.budget["nonInferenceEgressHosts"]});
    if contracts != original_contracts {
        verify_catalog_transition(&contracts, &original_contracts, now)?;
        require_adopted_transition(client, reg, source).await?;
    }
    settings.catalog(client, now).await?;
    let bound = reg
        .spec
        .first_install_funding
        .as_ref()
        .ok_or(BudgetError::Authorization)?;
    for (name, uid) in [
        (
            reg.spec.controller.namespace.name.as_str(),
            &bound.grant.root.workspace_uid,
        ),
        ("kube-system", &bound.grant.root.cluster_uid),
        (
            reg.spec.runtime_namespace.name.as_str(),
            &reg.spec.runtime_namespace.uid,
        ),
    ] {
        let namespace = Api::<Namespace>::all(client.clone())
            .get(name)
            .await
            .map_err(|_| BudgetError::Authorization)?;
        if namespace.uid().as_ref() != Some(uid) || namespace.metadata.deletion_timestamp.is_some()
        {
            return Err(BudgetError::Identity.into());
        }
    }
    validate_source_spec(source, expected(&intent, "KarsSandbox")?)?;
    let inference = Api::<InferencePolicy>::namespaced(client.clone(), &intent.namespace)
        .get("sre-inference")
        .await
        .map_err(|_| BudgetError::Authorization)?;
    let tool = Api::<ToolPolicy>::namespaced(client.clone(), &intent.namespace)
        .get("sre-tools")
        .await
        .map_err(|_| BudgetError::Authorization)?;
    let desired_inference: InferencePolicy =
        serde_json::from_value(expected(&intent, "InferencePolicy")?)
            .map_err(|_| BudgetError::Authorization)?;
    let desired_tool: ToolPolicy = serde_json::from_value(expected(&intent, "ToolPolicy")?)
        .map_err(|_| BudgetError::Authorization)?;
    let current_policy =
        serde_json::to_value(inference.spec).map_err(|_| BudgetError::Authorization)?;
    let original_policy =
        serde_json::to_value(desired_inference.spec).map_err(|_| BudgetError::Authorization)?;
    if current_policy != original_policy {
        verify_adopted_policy_update(
            &current_policy,
            &original_policy,
            bound.grant.unbounded_tokens,
        )?;
        require_adopted_transition(client, reg, source).await?;
    }
    if inference.metadata.deletion_timestamp.is_some()
        || tool.metadata.deletion_timestamp.is_some()
        || serde_json::to_value(tool.spec).map_err(|_| BudgetError::Authorization)?
            != serde_json::to_value(desired_tool.spec).map_err(|_| BudgetError::Authorization)?
    {
        return Err(BudgetError::Authorization.into());
    }
    Ok(bound.grant.clone())
}

async fn authorize_funding(
    client: &Client,
    settings: &Settings,
    source: &KarsSandbox,
    grant: &BuiltinFunding,
) -> Result<(), StoreError> {
    use super::store::Store;
    let now = chrono::Utc::now().timestamp();
    let Some(bound) = &source.spec.inference_budget_ref else {
        grant.validate(now)?;
        return Ok(());
    };
    if *bound != super::builtin::binding(grant, bound.account.clone())?
        || bound.account.namespace != settings.accounting_namespace
        || bound.account.name != super::account::name_for_root(&grant.root)
    {
        return Err(BudgetError::Identity.into());
    }
    let store = Store::new(client.clone(), &settings.accounting_namespace);
    let account = store.read_anchor(&grant.root, &bound.account.uid).await?;
    if account.spec.limits.normalized() != grant.limits.normalized() {
        return Err(BudgetError::Authorization.into());
    }
    if account
        .annotations()
        .get(super::account::BOOTSTRAP)
        .map(String::as_str)
        == Some("pending")
        || Store::ledger(&account).is_ok_and(|ledger| ledger.nodes.is_empty())
    {
        // Only the original, unexpired grant can finish an interrupted bootstrap.
        grant.validate(now)?;
        Store::bootstrap_ledger(&account)?;
        return Ok(());
    }
    super::topups::reconcile(client, settings, &store, &grant.root, &bound.account.uid).await?;
    let account = store.read(&grant.root, &bound.account.uid).await?;
    let ledger = Store::ledger(&account)?;
    if ledger.phase != crate::inference_budget_contract::AccountPhase::Active {
        return Err(BudgetError::Closed.into());
    }
    super::builtin::verify_grant(grant, ledger, source, chrono::Utc::now().timestamp())
}

fn verify_renewal_pin(
    grant: &crate::inference_budget_contract::ledger::TokenTopUp,
    reg: &KarsSRERegistration,
) -> Result<(), BudgetError> {
    let renewal = grant
        .funding_renewal
        .as_ref()
        .ok_or(BudgetError::Authorization)?;
    let original = reg
        .spec
        .first_install_funding
        .as_ref()
        .ok_or(BudgetError::Authorization)?;
    if renewal.bridge_reactivation.is_some()
        || reg.uid().as_ref() != Some(&renewal.registration_uid)
        || original.intent.uid != renewal.intent_uid
        || original.intent.digest != renewal.intent_digest
        || original.grant.expires_at != renewal.original_expires_at
        || original.grant.root != grant.root
        || original.grant.limits.normalized() != grant.original_limits
    {
        return Err(BudgetError::Authorization);
    }
    Ok(())
}

/// Re-run the sealed intent and actual registration/source checks on every CAS
/// retry. No privacy credential is needed to recover expired funding authority.
pub(super) async fn verify_renewal_intents(
    client: &Client,
    settings: &Settings,
    catalog: &ConfigMap,
    grants: &std::collections::BTreeMap<
        String,
        crate::inference_budget_contract::ledger::TokenTopUp,
    >,
    root: &crate::inference_budget_contract::RootIdentity,
    account_uid: &str,
) -> Result<(), StoreError> {
    let relevant: Vec<_> = grants
        .values()
        .filter(|grant| {
            grant.funding_renewal.is_some()
                && (grant.account_uid == account_uid
                    || (grant.root.kind == root.kind
                        && grant.root.resource.namespace == root.resource.namespace
                        && grant.root.resource.name == root.resource.name))
        })
        .collect();
    if relevant.is_empty() {
        return Ok(());
    }
    if root.kind == crate::inference_budget_contract::RootKind::KarsSandbox
        && root.resource.name == "bridge-orchestrator"
    {
        let source = Api::<KarsSandbox>::namespaced(client.clone(), &root.resource.namespace)
            .get(&root.resource.name)
            .await
            .map_err(|_| BudgetError::Authorization)?;
        for grant in relevant {
            if grant.account_uid != account_uid || grant.root != *root {
                return Err(BudgetError::Identity.into());
            }
            super::builtin::verify_bridge_renewal(
                grant,
                catalog,
                &source,
                &settings.accounting_namespace,
            )?;
        }
        return Ok(());
    }
    let reg = Api::<KarsSRERegistration>::all(client.clone())
        .get(crate::sre_registration::NAME)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    let source = Api::<KarsSandbox>::namespaced(client.clone(), &root.resource.namespace)
        .get(&root.resource.name)
        .await
        .map_err(|_| BudgetError::Authorization)?;
    let original = validate(client, settings, &source, catalog, &reg).await?;
    let bound = source
        .spec
        .inference_budget_ref
        .as_ref()
        .ok_or(BudgetError::Authorization)?;
    if original.root != *root
        || bound.account.uid != account_uid
        || bound.account.namespace != settings.accounting_namespace
        || bound.account.name != super::account::name_for_root(root)
        || *bound != super::builtin::binding(&original, bound.account.clone())?
    {
        return Err(BudgetError::Identity.into());
    }
    for grant in relevant {
        if grant.account_uid != account_uid {
            return Err(BudgetError::Identity.into());
        }
        verify_renewal_pin(grant, &reg)?;
    }
    Ok(())
}

/// Runs before private credentials are minted and on every authority refresh.
/// Retirement deliberately runs before this check, even after intent expiry.
pub(crate) async fn verify_registration(
    client: &Client,
    reg: &KarsSRERegistration,
    source: &KarsSandbox,
) -> Result<(), String> {
    if reg.spec.first_install_funding.is_none() && !source.annotations().contains_key(ANNOTATION) {
        return Ok(());
    }
    let settings = Settings::from_env()
        .map_err(|_| "First-install funding configuration is invalid")?
        .ok_or("First-install funding requires the governed broker")?;
    let catalog = settings
        .operator_config(client)
        .await
        .map_err(|_| "First-install funding catalog is unavailable")?;
    let result = async {
        let grant = validate(client, &settings, source, &catalog, reg).await?;
        authorize_funding(client, &settings, source, &grant).await
    }
    .await;
    result.map_err(|_| {
        "First-install SRE intent, identity, catalog or funding authorization failed verification".into()
    })
}

#[cfg(test)]
#[path = "first_install_tests.rs"]
mod tests;
