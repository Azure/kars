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
    if unbounded_tokens {
        if let Some(original_budget) = original.get("tokenBudget").and_then(Value::as_object) {
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
mod tests {
    use super::*;

    #[test]
    fn adopted_policy_updates_preserve_all_other_fields() {
        let original = json!({
            "appliesTo": {"sandboxName":"sre"},
            "tokenBudget": {"dailyTokens":1500000,"perRequestTokens":528000},
            "modelPreference": {"primary":{"provider":"azure-openai","deployment":"model"},"fallback":[]},
            "contentSafety": {"requirePromptShields":true}
        });
        assert!(verify_adopted_policy_update(&original, &original, false).is_ok());
        for daily in [1500000, 5000000] {
            let mut selected = original.clone();
            selected["tokenBudget"]["dailyTokens"] = json!(daily);
            selected["modelPreference"]["primary"] = json!({
                "provider":"github-copilot", "deployment":"claude-opus-5.5"
            });
            let before = selected.clone();
            assert!(verify_adopted_policy_update(&selected, &original, false).is_ok());
            assert_eq!(selected, before);
            for value in [
                json!(0),
                json!(-1),
                json!(1499999),
                json!(1.5),
                json!("5000000"),
                Value::Null,
                json!(9007199254740992_u64),
            ] {
                let mut changed = selected.clone();
                changed["tokenBudget"]["dailyTokens"] = value;
                assert!(verify_adopted_policy_update(&changed, &original, false).is_err());
            }
            for (path, value) in [
                ("/tokenBudget/perRequestTokens", json!(0)),
                ("/appliesTo/sandboxName", json!("other")),
                (
                    "/modelPreference/fallback",
                    json!([{"provider":"other","deployment":"other"}]),
                ),
                ("/contentSafety/requirePromptShields", json!(false)),
            ] {
                let mut changed = selected.clone();
                *changed.pointer_mut(path).unwrap() = value;
                assert!(verify_adopted_policy_update(&changed, &original, false).is_err());
            }
            for primary in [
                json!({"provider":"", "deployment":"model"}),
                json!({"provider":"bad/tag", "deployment":"model"}),
                json!({"provider":"a".repeat(65), "deployment":"model"}),
                json!({"provider":"github-copilot", "deployment":" "}),
                json!({"provider":"github-copilot", "deployment":"line\nbreak"}),
                json!({"provider":"github-copilot", "deployment":"a".repeat(257)}),
                json!({"provider":"github-copilot"}),
                json!({"provider":"github-copilot", "deployment":"model", "extra":true}),
                Value::Null,
            ] {
                let mut changed = selected.clone();
                changed["modelPreference"]["primary"] = primary;
                assert!(verify_adopted_policy_update(&changed, &original, false).is_err());
            }
            selected["tokenBudget"]["monthlyTokens"] = json!(0);
            assert!(verify_adopted_policy_update(&selected, &original, false).is_err());
            let mut unlimited = original.clone();
            unlimited["tokenBudget"]["dailyTokens"] = json!(0);
            assert!(verify_adopted_policy_update(&before, &unlimited, false).is_err());
        }
        let mut daily_only = original.clone();
        daily_only["tokenBudget"]["dailyTokens"] = json!(5000000);
        assert!(verify_adopted_policy_update(&daily_only, &original, false).is_ok());
    }

    #[test]
    fn adopted_unbounded_funding_allows_only_removing_token_ceilings() {
        let original = json!({
            "appliesTo": {"sandboxName":"sre"},
            "tokenBudget": {"dailyTokens":1500000,"monthlyTokens":9000000,"perRequestTokens":528000},
            "modelPreference": {"primary":{"provider":"azure-foundry","deployment":"model"},"fallback":[]},
            "contentSafety": {"requirePromptShields":true}
        });
        for mask in 1..8 {
            let mut selected = original.clone();
            for (i, field) in ["dailyTokens", "monthlyTokens", "perRequestTokens"]
                .iter()
                .enumerate()
            {
                if mask & (1 << i) != 0 {
                    selected["tokenBudget"]
                        .as_object_mut()
                        .unwrap()
                        .remove(*field);
                }
            }
            assert!(verify_adopted_policy_update(&selected, &original, true).is_ok());
            assert!(verify_adopted_policy_update(&selected, &original, false).is_err());
            for (path, value) in [
                ("/appliesTo/sandboxName", json!("other")),
                ("/contentSafety/requirePromptShields", json!(false)),
                (
                    "/modelPreference/fallback",
                    json!([{"provider":"other","deployment":"other"}]),
                ),
            ] {
                let mut changed = selected.clone();
                *changed.pointer_mut(path).unwrap() = value;
                assert!(verify_adopted_policy_update(&changed, &original, true).is_err());
            }
            selected["tokenBudget"]["unknown"] = json!(true);
            assert!(verify_adopted_policy_update(&selected, &original, true).is_err());
        }
        let mut uncapped = original.clone();
        uncapped.as_object_mut().unwrap().remove("tokenBudget");
        assert!(verify_adopted_policy_update(&uncapped, &original, true).is_ok());
        assert!(verify_adopted_policy_update(&uncapped, &original, false).is_err());
        for unbounded in [false, true] {
            assert!(verify_adopted_policy_update(&uncapped, &uncapped, unbounded).is_ok());
        }
        for value in [json!(0), json!(-1), json!(1.5), json!("528000")] {
            let mut selected = original.clone();
            selected["tokenBudget"]["perRequestTokens"] = value;
            assert!(verify_adopted_policy_update(&selected, &original, true).is_err());
        }
    }

    #[test]
    fn adopted_unbounded_cap_removal_survives_typed_policy_serialization() {
        let typed = |spec: Value| {
            let spec: crate::inference_policy::InferencePolicySpec =
                serde_json::from_value(spec).unwrap();
            serde_json::to_value(spec).unwrap()
        };
        for budget in [
            json!({"dailyTokens":30000000,"perRequestTokens":528000}),
            json!({"perRequestTokens":528000}),
        ] {
            let original = json!({
                "appliesTo":{"sandboxName":"sre"},
                "tokenBudget":budget,
                "modelPreference":{"primary":{"provider":"azure-openai","deployment":"model"},"fallback":[]},
                "contentSafety":{"requirePromptShields":true}
            });
            let original = typed(original);
            assert!(verify_adopted_policy_update(&original, &original, false).is_ok());
            for mask in 1..8 {
                let mut selected = original.clone();
                for (i, field) in ["dailyTokens", "monthlyTokens", "perRequestTokens"]
                    .iter()
                    .enumerate()
                {
                    if mask & (1 << i) != 0 {
                        selected["tokenBudget"]
                            .as_object_mut()
                            .unwrap()
                            .remove(*field);
                    }
                }
                selected["modelPreference"]["primary"] = json!({
                    "provider":"github-copilot", "deployment":"claude-opus-5.5"
                });
                let selected = typed(selected);
                let before = selected.clone();
                assert!(verify_adopted_policy_update(&selected, &original, true).is_ok());
                assert_eq!(selected, before);
                let removed = selected["tokenBudget"] != original["tokenBudget"];
                assert_eq!(
                    verify_adopted_policy_update(&selected, &original, false).is_err(),
                    removed
                );
                let mut drift = selected.clone();
                drift["contentSafety"]["requirePromptShields"] = json!(false);
                assert!(verify_adopted_policy_update(&drift, &original, true).is_err());
            }
            let mut selected = original.clone();
            selected.as_object_mut().unwrap().remove("tokenBudget");
            let selected = typed(selected);
            assert!(verify_adopted_policy_update(&selected, &original, true).is_ok());
            assert!(verify_adopted_policy_update(&selected, &original, false).is_err());
            assert!(verify_adopted_policy_update(&selected, &selected, true).is_ok());
        }
    }

    fn rendered_source() -> (Value, Value) {
        // These blocks contain no Helm expressions: read the maintained renderer
        // and CRD rather than duplicating their security defaults in a fixture.
        let template = include_str!("../../../deploy/helm/kars/templates/_sre-resources.tpl");
        let sandbox: Value = serde_yaml::from_str(
            template
                .split_once("  sandbox:\n")
                .unwrap()
                .1
                .split_once("\n  inferenceRef:")
                .unwrap()
                .0,
        )
        .unwrap();
        let crd = include_str!("../../../deploy/helm/kars/templates/crd.yaml");
        let schema: Value = serde_yaml::from_str(
            crd.split_once("                sandbox:\n")
                .unwrap()
                .1
                .split_once("                inferenceRef:")
                .unwrap()
                .0,
        )
        .unwrap();
        (
            json!({"apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsSandbox",
            "metadata":{"name":"sre","namespace":"kars-system"},
            "spec":{"runtime":{"kind":"Hermes","hermes":{"extraEnv":{"SRE_ENABLED":"true"}}},
                "sandbox":sandbox,"inferenceRef":{"name":"sre-inference"}}}),
            schema,
        )
    }

    fn api_default_sandbox(source: &mut Value, schema: &Value) {
        for (key, property) in schema["properties"].as_object().unwrap() {
            if let Some(default) = property.get("default") {
                source["spec"]["sandbox"]
                    .as_object_mut()
                    .unwrap()
                    .entry(key.clone())
                    .or_insert_with(|| default.clone());
            }
        }
    }

    #[test]
    fn rendered_sre_security_survives_actual_crd_defaulting() {
        let (desired, schema) = rendered_source();
        let mut live = desired.clone();
        api_default_sandbox(&mut live, &schema);
        assert_eq!(live, desired);
        let source: KarsSandbox = serde_json::from_value(live).unwrap();
        assert!(validate_source_spec(&source, desired.clone()).is_ok());

        // Reproduce the original partial object and both real API/serde mismatches.
        let mut partial = desired;
        partial["spec"]["sandbox"] = json!({"isolation":"standard"});
        let raw: KarsSandbox = serde_json::from_value(partial.clone()).unwrap();
        let mut defaulted = partial.clone();
        api_default_sandbox(&mut defaulted, &schema);
        let defaulted: KarsSandbox = serde_json::from_value(defaulted).unwrap();
        assert_ne!(
            raw.spec.sandbox.as_ref().unwrap().writable_paths,
            defaulted.spec.sandbox.as_ref().unwrap().writable_paths
        );
        assert_ne!(
            raw.spec.sandbox.as_ref().unwrap().selinux_context,
            defaulted.spec.sandbox.as_ref().unwrap().selinux_context
        );
        assert!(validate_source_spec(&defaulted, partial).is_err());
    }

    #[test]
    fn defaulted_sre_security_mutations_are_not_normalized_away() {
        let (desired, schema) = rendered_source();
        for (path, value) in [
            ("/spec/sandbox/isolation", json!("enhanced")),
            ("/spec/sandbox/seccompProfile", json!("other")),
            ("/spec/sandbox/selinuxContext", json!("other_t")),
            ("/spec/sandbox/readOnlyRootFilesystem", json!(false)),
            ("/spec/sandbox/runAsNonRoot", json!(false)),
            ("/spec/sandbox/allowPrivilegeEscalation", json!(true)),
            (
                "/spec/sandbox/writablePaths",
                json!(["/sandbox", "/tmp", "/etc"]),
            ),
            ("/spec/sandbox/writablePaths", json!(null)),
            ("/spec/inferenceRef/name", json!("other")),
            ("/spec/runtime/kind", json!("OpenClaw")),
        ] {
            let mut live = desired.clone();
            api_default_sandbox(&mut live, &schema);
            *live.pointer_mut(path).unwrap() = value;
            let source: KarsSandbox = serde_json::from_value(live).unwrap();
            assert!(
                validate_source_spec(&source, desired.clone()).is_err(),
                "{path}"
            );
        }
    }

    pub(super) fn fixture() -> (ConfigMap, KarsSRERegistration) {
        let encoded = json!({"version":1,"release":"kars","namespace":"kars-system",
            "valuesDigest":"a".repeat(64), "funding":{"tokens":1000,"expiresAt":"2999-01-01T00:00:00Z"},
            "budget":{"enabled":true}, "resources":[
                {"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox","metadata":{"name":"sre","namespace":"kars-system"}},
                {"apiVersion":"kars.azure.com/v1alpha1","kind":"InferencePolicy","metadata":{"name":"sre-inference","namespace":"kars-system"}},
                {"apiVersion":"kars.azure.com/v1alpha1","kind":"ToolPolicy","metadata":{"name":"sre-tools","namespace":"kars-system"}}
            ]}).to_string();
        let map = serde_json::from_value(json!({"apiVersion":"v1","kind":"ConfigMap","immutable":true,
            "metadata":{"name":NAME,"namespace":"kars-system","uid":"intent-uid","resourceVersion":"1",
                "annotations":{"meta.helm.sh/release-name":"kars","meta.helm.sh/release-namespace":"kars-system"}},
            "data":{"intent.json":encoded}})).unwrap();
        let reg = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSRERegistration",
            "metadata":{"name":"canonical","uid":"registration-uid"},"spec":{
                "controller":{"namespace":{"name":"kars-system","uid":"workspace-uid"},
                    "deployment":{"name":"kars-controller","uid":"controller-uid"},"release":"kars"},
                "sandbox":{"namespace":"kars-system","name":"sre","uid":"source-uid"},
                "runtimeNamespace":{"name":"kars-sre","uid":"runtime-uid"},
                "firstInstallFunding":{"intent":{"name":NAME,"uid":"intent-uid",
                    "digest":crate::providers::signing::sha256_hex(encoded.as_bytes())},
                    "grant":{"root":{"kind":"KarsSandbox","resource":{"namespace":"kars-system","name":"sre","uid":"source-uid"},
                        "workspaceUid":"workspace-uid","clusterUid":"cluster-uid"},
                        "limits":{"tokens":1000},"expiresAt":"2999-01-01T00:00:00Z"}}
            }})).unwrap();
        (map, reg)
    }

    #[test]
    fn contract_renewal_preserves_every_sealed_field_and_input() {
        let original = json!({"version":"catalog-v1", "nonInferenceEgressHosts":[], "contracts":[{
            "id":"chat", "version":"v1", "validUntil":"2020-01-01T00:00:00Z",
            "providerId":"azure-openai", "endpoint":"https://models.example", "model":"model",
            "operation":"ChatCompletions", "outputField":"MaxCompletionTokens",
            "maximumInputTokens":400000, "maximumOutputTokens":128000,
            "maximumWireBytes":1048576, "outputBoundIncludesReasoning":true
        }]});
        let mut renewed = original.clone();
        renewed["version"] = json!("catalog-v2");
        renewed["contracts"][0]["version"] = json!("v2");
        renewed["contracts"][0]["validUntil"] = json!("2999-01-01T00:00:00Z");
        let before = renewed.clone();
        let now = 1_800_000_000;
        verify_contract_renewal(&renewed, &original, now).unwrap();
        assert_eq!(renewed, before);
        assert!(verify_contract_renewal(&original, &original, now).is_err());
        for (path, value) in [
            ("/version", json!("catalog-v1")),
            ("/version", json!(null)),
            ("/nonInferenceEgressHosts", json!(["new.example"])),
            ("/contracts/0/id", json!("other")),
            ("/contracts/0/providerId", json!("other")),
            ("/contracts/0/endpoint", json!("https://other.example")),
            ("/contracts/0/model", json!("other")),
            ("/contracts/0/operation", json!("Responses")),
            ("/contracts/0/outputField", json!("MaxTokens")),
            ("/contracts/0/maximumInputTokens", json!(500000)),
            ("/contracts/0/maximumOutputTokens", json!(256000)),
            ("/contracts/0/maximumWireBytes", json!(2097152)),
            ("/contracts/0/outputBoundIncludesReasoning", json!(false)),
            ("/contracts/0/version", json!("v1")),
            ("/contracts/0/version", json!("")),
            ("/contracts/0/version", json!(123)),
        ] {
            let mut changed = renewed.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(
                verify_contract_renewal(&changed, &original, now).is_err(),
                "{path}"
            );
        }
        for expiry in [
            "",
            "2019-01-01T00:00:00Z",
            "2020-01-01T00:00:00Z",
            "2999-02-30T00:00:00Z",
            "2999-01-01T00:00:00+00:00",
            "2999-01-01T00:00:00.0Z",
        ] {
            let mut changed = renewed.clone();
            changed["contracts"][0]["validUntil"] = json!(expiry);
            assert!(
                verify_contract_renewal(&changed, &original, now).is_err(),
                "{expiry}"
            );
        }
        for contracts in [
            json!([]),
            json!([renewed["contracts"][0], renewed["contracts"][0]]),
            json!("bad"),
        ] {
            let mut changed = renewed.clone();
            changed["contracts"] = contracts;
            assert!(verify_contract_renewal(&changed, &original, now).is_err());
        }
        for field in ["maximumPrice", "unexpected"] {
            let mut changed = renewed.clone();
            changed["contracts"][0][field] = json!({"maximumMicros":1});
            assert!(verify_contract_renewal(&changed, &original, now).is_err());
        }
        let mut unchanged = original.clone();
        unchanged["version"] = json!("catalog-v2");
        assert!(verify_contract_renewal(&unchanged, &original, now).is_err());
        verify_contract_renewal(&renewed, &original, i64::MAX).unwrap();
    }

    #[test]
    fn catalog_transition_classifies_non_model_hosts_without_changing_authority() {
        let original = json!({"version":"catalog-v1", "nonInferenceEgressHosts":[], "contracts":[{
            "id":"chat", "version":"v1", "validUntil":"2999-01-01T00:00:00Z",
            "providerId":"azure-openai", "endpoint":"https://models.example", "model":"model",
            "operation":"ChatCompletions", "outputField":"MaxCompletionTokens",
            "maximumInputTokens":400000, "maximumOutputTokens":128000,
            "maximumWireBytes":1048576, "outputBoundIncludesReasoning":true
        }]});
        let now = 1_800_000_000;
        let mut classified = original.clone();
        classified["nonInferenceEgressHosts"] = json!(["www.zendesk.com", "www.intercom.com"]);
        let before = classified.clone();
        verify_catalog_transition(&classified, &original, now).unwrap();
        assert_eq!(classified, before);
        verify_catalog_transition(&original, &classified, now).unwrap();
        for hosts in [
            json!(["models.example"]),
            json!(["*.example"]),
            json!(["UPPER.example"]),
            json!(["www.example:443"]),
            json!(["https://www.example"]),
            json!(null),
            json!([false]),
            json!([""]),
            json!(vec!["www.example"; 65]),
        ] {
            let mut changed = classified.clone();
            changed["nonInferenceEgressHosts"] = hosts;
            assert!(verify_catalog_transition(&changed, &original, now).is_err());
        }
        for (path, value) in [
            ("/contracts/0/model", json!("other")),
            ("/contracts/0/maximumInputTokens", json!(500000)),
            ("/contracts/0/endpoint", json!("https://other.example")),
            ("/version", json!("catalog-v2")),
        ] {
            let mut changed = classified.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(
                verify_catalog_transition(&changed, &original, now).is_err(),
                "{path}"
            );
        }
        let mut renewed = classified.clone();
        renewed["version"] = json!("catalog-v2");
        renewed["contracts"][0]["version"] = json!("v2");
        renewed["contracts"][0]["validUntil"] = json!("2999-02-01T00:00:00Z");
        verify_catalog_transition(&renewed, &original, now).unwrap();
        renewed["unexpected"] = json!(true);
        assert!(verify_catalog_transition(&renewed, &original, now).is_err());
    }

    fn additional_model_catalog() -> (Value, Value) {
        let original = json!({"version":"catalog-v1", "nonInferenceEgressHosts":[], "contracts":[{
            "id":"chat", "version":"v1", "validUntil":"2999-01-01T00:00:00Z",
            "providerId":"azure-openai", "endpoint":"https://models.example", "model":"model",
            "operation":"ChatCompletions", "outputField":"MaxCompletionTokens",
            "maximumInputTokens":400000, "maximumOutputTokens":128000,
            "maximumWireBytes":1048576, "outputBoundIncludesReasoning":true
        }]});
        let mut current = original.clone();
        current["version"] = json!("catalog-v2");
        let mut addition = original["contracts"][0].clone();
        addition["id"] = json!("full-chat");
        addition["model"] = json!("full-model");
        addition["maximumInputTokens"] = json!(1050000);
        current["contracts"]
            .as_array_mut()
            .unwrap()
            .push(addition.clone());
        addition["id"] = json!("full-responses");
        addition["operation"] = json!("Responses");
        addition["outputField"] = json!("MaxOutputTokens");
        current["contracts"].as_array_mut().unwrap().push(addition);
        (original, current)
    }

    #[test]
    fn additive_catalog_preserves_original_authority_with_or_without_renewal() {
        for renew in [false, true] {
            let (original, mut current) = additional_model_catalog();
            if renew {
                current["contracts"][0]["version"] = json!("renewed-v2");
                current["contracts"][0]["validUntil"] = json!("2999-02-01T00:00:00Z");
            }
            let before = current.clone();
            verify_catalog_transition(&current, &original, 1_800_000_000).unwrap();
            assert_eq!(current, before);
        }
    }

    #[test]
    fn retained_catalog_expiry_is_enforced_per_selected_route() {
        use crate::inference_budget_contract::catalog::Catalog;
        let now = 1_800_000_000;
        for expired_index in [0, 1] {
            let (mut original, mut current) = additional_model_catalog();
            original["contracts"][0]["validUntil"] = json!("2020-01-01T00:00:00Z");
            current["contracts"][0]["version"] = json!("renewed-v2");
            current["contracts"][expired_index]["validUntil"] = json!("2021-01-01T00:00:00Z");
            let before = current.clone();
            verify_catalog_transition(&current, &original, now).unwrap();
            assert_eq!(current, before);
            let catalog: Catalog = serde_json::from_value(current).unwrap();
            for (index, contract) in catalog.contracts.iter().enumerate() {
                let selected = catalog.select(
                    &contract.provider_id,
                    &contract.endpoint,
                    &contract.model,
                    contract.operation,
                    now,
                );
                assert_eq!(selected.is_ok(), index != expired_index);
            }
            assert!(
                catalog
                    .select(
                        "azure-openai",
                        "https://models.example",
                        "missing-model",
                        crate::inference_budget_contract::tariffs::Operation::ChatCompletions,
                        now
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn additive_catalog_rejects_invalid_new_routes_and_original_authority_changes() {
        for (path, value) in [
            ("/version", json!("catalog-v1")),
            ("/contracts/0/maximumInputTokens", json!(500000)),
            ("/contracts/0/model", json!("replaced-model")),
            ("/contracts/0/endpoint", json!("https://other.example")),
            ("/contracts/1/id", json!("chat")),
            ("/contracts/1/model", json!("model")),
            ("/contracts/1/providerId", json!("")),
            ("/contracts/1/maximumInputTokens", json!(0)),
            ("/contracts/1/validUntil", json!("2000-02-30T00:00:00Z")),
            (
                "/contracts/1/validUntil",
                json!("2999-01-01T00:00:00+00:00"),
            ),
            ("/contracts/1/outputBoundIncludesReasoning", json!(false)),
            ("/contracts/1/outputField", json!("MaxOutputTokens")),
            ("/contracts/2/operation", json!("ChatCompletions")),
            ("/contracts/2/model", json!("model")),
        ] {
            let (original, mut current) = additional_model_catalog();
            *current.pointer_mut(path).unwrap() = value;
            assert!(
                verify_catalog_transition(&current, &original, 1_800_000_000).is_err(),
                "{path}"
            );
        }
        let (original, mut current) = additional_model_catalog();
        current["contracts"].as_array_mut().unwrap().remove(0);
        assert!(verify_catalog_transition(&current, &original, 1_800_000_000).is_err());
        let (original, mut current) = additional_model_catalog();
        current["unexpected"] = json!(true);
        assert!(verify_catalog_transition(&current, &original, 1_800_000_000).is_err());
    }

    #[test]
    fn contract_renewal_requires_live_immutable_owned_adoption_of_original_intent() {
        let (mut map, reg) = fixture();
        map.metadata.name = Some("kars-sre-helm-adoption".into());
        map.data = Some(std::collections::BTreeMap::from([
            ("intentUID".into(), "intent-uid".into()),
            ("valuesDigest".into(), "a".repeat(64)),
        ]));
        verify_contract_adoption(&map, &reg).unwrap();
        for field in [
            "uid",
            "owner",
            "intent",
            "digest",
            "mutable",
            "deleted",
            "namespace",
            "name",
        ] {
            let mut changed = map.clone();
            match field {
                "uid" => changed.metadata.uid = None,
                "owner" => changed.metadata.annotations = None,
                "intent" => {
                    changed
                        .data
                        .as_mut()
                        .unwrap()
                        .insert("intentUID".into(), "other".into());
                }
                "digest" => {
                    changed
                        .data
                        .as_mut()
                        .unwrap()
                        .insert("valuesDigest".into(), "bad".into());
                }
                "mutable" => changed.immutable = Some(false),
                "deleted" => {
                    changed.metadata.deletion_timestamp =
                        Some(serde_json::from_value(json!("2026-01-01T00:00:00Z")).unwrap())
                }
                "namespace" => changed.metadata.namespace = Some("other".into()),
                "name" => changed.metadata.name = Some("other".into()),
                _ => unreachable!(),
            }
            assert!(verify_contract_adoption(&changed, &reg).is_err(), "{field}");
        }
    }

    #[test]
    fn renewal_requires_exact_registration_original_grant_and_immutable_intent() {
        let (_, reg) = fixture();
        let original = reg.spec.first_install_funding.as_ref().unwrap();
        let grant = crate::inference_budget_contract::ledger::TokenTopUp {
            id: "renewal".into(),
            config_uid: "config-uid".into(),
            account_uid: "account-uid".into(),
            root: original.grant.root.clone(),
            original_limits: original.grant.limits,
            tokens: 100,
            expires_at: "2999-02-01T00:00:00Z".into(),
            nodes: Default::default(),
            funding_renewal: Some(crate::inference_budget_contract::ledger::FundingRenewal {
                bridge_reactivation: None,
                registration_uid: reg.uid().unwrap(),
                intent_uid: original.intent.uid.clone(),
                intent_digest: original.intent.digest.clone(),
                original_expires_at: original.grant.expires_at.clone(),
                previous_top_up_ids: vec![],
            }),
        };
        verify_renewal_pin(&grant, &reg).unwrap();
        for field in [
            "registration",
            "intent",
            "digest",
            "expiry",
            "root",
            "limits",
        ] {
            let mut changed = grant.clone();
            let pin = changed.funding_renewal.as_mut().unwrap();
            match field {
                "registration" => pin.registration_uid = "replaced".into(),
                "intent" => pin.intent_uid = "replaced".into(),
                "digest" => pin.intent_digest = "b".repeat(64),
                "expiry" => pin.original_expires_at = "2998-01-01T00:00:00Z".into(),
                "root" => changed.root.resource.uid = "replaced".into(),
                "limits" => changed.original_limits.tokens = Some(2000),
                _ => unreachable!(),
            }
            assert!(verify_renewal_pin(&changed, &reg).is_err(), "{field}");
        }
    }

    #[test]
    fn original_immutable_intent_resolves_without_reissuing_funding() {
        let (map, reg) = fixture();
        let original = serde_json::to_value(&reg).unwrap();
        assert_eq!(intent(&map, &reg, 100).unwrap().funding.tokens, Some(1000));
        assert_eq!(intent(&map, &reg, 101).unwrap().funding.tokens, Some(1000));
        assert_eq!(serde_json::to_value(&reg).unwrap(), original);
        assert!(intent(&map, &reg, 32_472_144_000).is_err());
    }

    #[test]
    fn missing_replaced_changed_or_conflicting_intent_fails_closed() {
        for fault in [
            "missing",
            "uid",
            "digest",
            "mutable",
            "owner",
            "amount",
            "expiry",
            "source",
            "workspace",
            "json",
            "usd",
        ] {
            let (mut map, mut reg) = fixture();
            match fault {
                "missing" => reg.spec.first_install_funding = None,
                "uid" => map.metadata.uid = Some("replacement".into()),
                "digest" => map
                    .data
                    .as_mut()
                    .unwrap()
                    .get_mut("intent.json")
                    .unwrap()
                    .push(' '),
                "mutable" => map.immutable = Some(false),
                "owner" => {
                    map.annotations_mut()
                        .insert("meta.helm.sh/release-name".into(), "foreign".into());
                }
                "amount" => {
                    reg.spec
                        .first_install_funding
                        .as_mut()
                        .unwrap()
                        .grant
                        .limits
                        .tokens = Some(2000)
                }
                "expiry" => {
                    reg.spec
                        .first_install_funding
                        .as_mut()
                        .unwrap()
                        .grant
                        .expires_at = "2998-01-01T00:00:00Z".into()
                }
                "source" => reg.spec.sandbox.uid = "replacement".into(),
                "workspace" => reg.spec.controller.namespace.uid = "replacement".into(),
                "json" => {
                    map.data
                        .as_mut()
                        .unwrap()
                        .insert("intent.json".into(), "{}".into());
                    reg.spec
                        .first_install_funding
                        .as_mut()
                        .unwrap()
                        .intent
                        .digest = crate::providers::signing::sha256_hex(b"{}");
                }
                "usd" => {
                    reg.spec
                        .first_install_funding
                        .as_mut()
                        .unwrap()
                        .grant
                        .limits
                        .usd_micros = Some(100)
                }
                _ => unreachable!(),
            }
            assert!(intent(&map, &reg, 100).is_err(), "{fault}");
        }
    }
}
