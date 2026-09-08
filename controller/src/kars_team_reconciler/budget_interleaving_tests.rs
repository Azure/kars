// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Full Team reconcile against a stateful API, with real ledger transitions.

use super::*;
use crate::inference_budget::{
    account::{
        BOOTSTRAP, KarsBudgetAccount, KarsBudgetAccountSpec, KarsBudgetAccountStatus, MANAGED_BY,
        OWNER, name_for_root,
    },
    config::Settings,
};
use crate::inference_budget_contract::{
    AccountReference, AttemptCommand, AttemptKey, BudgetScope, ExecutionIdentity, ReserveRequest,
    ResourceIdentity, RootIdentity, RootKind, Settlement, TaskAuthority, TaskBudgetBinding, Usage,
    ledger::Ledger,
    tariffs::{MaximumPrice, ModelContract, Operation, OutputField},
};
use crate::kars_task::{KarsTaskStatus, TaskBlueprint, TaskModel};
use crate::kars_team::TeamRole;
use std::sync::atomic::{AtomicBool, Ordering};

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct BudgetEnvironment(Vec<(&'static str, Option<String>)>);
impl BudgetEnvironment {
    fn enabled() -> Self {
        let mut old = Vec::new();
        for (key, value) in [
            ("KARS_INFERENCE_BUDGET_ENABLED", "true".into()),
            ("KARS_INFERENCE_BUDGET_TLS_SECRET", "fixture-tls".into()),
            (
                "KARS_INFERENCE_BUDGET_ROUTER_DIGEST",
                format!("sha256:{}", "a".repeat(64)),
            ),
        ] {
            old.push((key, std::env::var(key).ok()));
            // This module serializes its configuration mutations and restores
            // them on unwind; run its targeted selector with --test-threads=1.
            unsafe { std::env::set_var(key, value) };
        }
        Self(old)
    }
}
impl Drop for BudgetEnvironment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn contract() -> ModelContract {
    ModelContract {
        id: "fixture".into(),
        version: "v1".into(),
        valid_until: "2030-01-01T00:00:00Z".into(),
        provider_id: "azure-openai".into(),
        endpoint: "https://fixture.example".into(),
        model: "reviewed-model".into(),
        operation: Operation::ChatCompletions,
        output_field: OutputField::MaxTokens,
        maximum_input_tokens: 10,
        maximum_output_tokens: 20,
        maximum_wire_bytes: 4096,
        output_bound_includes_reasoning: true,
        maximum_price: Some(MaximumPrice::PerRequest { maximum_micros: 5 }),
    }
}

#[derive(Clone)]
struct BudgetApis {
    account: Arc<Mutex<Option<KarsBudgetAccount>>>,
    catalog_failed: Arc<AtomicBool>,
    store_failed: Arc<AtomicBool>,
    namespace: String,
}
impl Respond for BudgetApis {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if request.method != "GET" {
            return failure(405);
        }
        let path = request.url.path();
        if path.contains("/admissionregistration.k8s.io/") {
            let name = path.rsplit('/').next().unwrap();
            let bundle: Value = serde_json::from_str(
                &include_str!("../../../deploy/helm/kars/files/inference-budget-admission.json")
                    .replace("__ACCOUNTING_NAMESPACE__", &self.namespace),
            )
            .unwrap();
            let policy = bundle["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|policy| policy["name"] == name)
                .unwrap();
            let (kind, spec) = if path.contains("/validatingadmissionpolicybindings/") {
                (
                    "ValidatingAdmissionPolicyBinding",
                    json!({"policyName":name,"validationActions":["Deny","Audit"]}),
                )
            } else {
                ("ValidatingAdmissionPolicy", policy["spec"].clone())
            };
            return response(
                200,
                json!({
                    "apiVersion":"admissionregistration.k8s.io/v1","kind":kind,
                    "metadata":{"name":name,"uid":format!("uid-{name}"),"generation":1,"resourceVersion":"1"},
                    "spec":spec,"status":{"observedGeneration":1,"typeChecking":{"expressionWarnings":[]}}
                }),
            );
        }
        if path.ends_with("/configmaps/kars-inference-budget-contracts") {
            if self.catalog_failed.load(Ordering::SeqCst) {
                return failure(503);
            }
            return response(
                200,
                json!({
                    "apiVersion":"v1","kind":"ConfigMap",
                    "metadata":{"name":"kars-inference-budget-contracts","namespace":self.namespace,
                        "uid":"catalog-uid","resourceVersion":"1",
                        "annotations":{"kars.azure.com/inference-budget-contracts":"v1"}},
                    "data":{"contracts.json":serde_json::to_string(&json!({
                        "version":"v1","contracts":[contract()],"nonInferenceEgressHosts":[]
                    })).unwrap()}
                }),
            );
        }
        if self.store_failed.load(Ordering::SeqCst) {
            return failure(503);
        }
        match self.account.lock().unwrap().as_ref() {
            Some(account) => response(200, serde_json::to_value(account).unwrap()),
            None => failure(404),
        }
    }
}

async fn budget_apis(server: &MockServer) -> BudgetApis {
    let settings = Settings::from_env().unwrap().unwrap();
    let apis = BudgetApis {
        account: Arc::new(Mutex::new(None)),
        catalog_failed: Arc::new(AtomicBool::new(false)),
        store_failed: Arc::new(AtomicBool::new(false)),
        namespace: settings.accounting_namespace,
    };
    for prefix in [
        "/apis/admissionregistration.k8s.io/v1/".to_string(),
        format!(
            "/api/v1/namespaces/{}/configmaps/kars-inference-budget-contracts",
            apis.namespace
        ),
        format!(
            "/apis/kars.azure.com/v1alpha1/namespaces/{}/karsbudgetaccounts/",
            apis.namespace
        ),
    ] {
        Mock::given(wiremock::matchers::path_regex(format!(
            "^{}",
            regex::escape(&prefix)
        )))
        .respond_with(apis.clone())
        .with_priority(1)
        .mount(server)
        .await;
    }
    apis
}

fn latest(store: &Arc<Mutex<Store>>) -> KarsTeam {
    serde_json::from_value(store.lock().unwrap().team.clone()).unwrap()
}
fn ids(store: &Arc<Mutex<Store>>) -> BTreeMap<String, String> {
    store
        .lock()
        .unwrap()
        .tasks
        .iter()
        .map(|(name, task)| {
            (
                name.clone(),
                task["metadata"]["uid"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}
async fn cycle(client: &Client, store: &Arc<Mutex<Store>>) {
    reconcile(
        Arc::new(latest(store)),
        Arc::new(Ctx {
            client: client.clone(),
        }),
    )
    .await
    .unwrap();
}

fn enroll(store: &Arc<Mutex<Store>>, apis: &BudgetApis) {
    let mut state = store.lock().unwrap();
    let team: KarsTeam = serde_json::from_value(state.team.clone()).unwrap();
    let principal = specs::principal_name(&team);
    let root_uid = state.tasks[&principal]["metadata"]["uid"]
        .as_str()
        .unwrap()
        .to_string();
    let root = RootIdentity {
        kind: RootKind::KarsTeam,
        resource: ResourceIdentity {
            namespace: "tenant-a".into(),
            name: team.name_any(),
            uid: team.uid().unwrap(),
        },
        workspace_uid: "workspace-uid".into(),
        cluster_uid: "cluster-uid".into(),
    };
    let account_ref = AccountReference {
        namespace: apis.namespace.clone(),
        name: name_for_root(&root),
        uid: "account-uid".into(),
    };
    let limits = crate::inference_budget::binding::limits(&team.spec.envelope).unwrap();
    let prior = apis.account.lock().unwrap().clone();
    let mut ledger = prior
        .and_then(|account| account.status.and_then(|status| status.ledger))
        .unwrap_or_else(|| Ledger::new(account_ref.uid.clone(), root.clone(), limits).unwrap());
    let mut names: Vec<_> = state.tasks.keys().cloned().collect();
    names.sort_by_key(|name| name != &principal);
    for name in names {
        let mut task: KarsTask = serde_json::from_value(state.tasks[&name].clone()).unwrap();
        let parent_uid = (name != principal).then(|| root_uid.clone());
        let model = crate::kars_task::blueprint::controller_default_model();
        let authority = TaskAuthority {
            task: ResourceIdentity {
                namespace: "tenant-a".into(),
                name: name.clone(),
                uid: task.uid().unwrap(),
            },
            parent_uid: parent_uid.clone(),
            root_task_uid: root_uid.clone(),
            authorization_digest: task.spec.authorization_digest_with_model(&model),
            effective_authorization: task.spec.authorization_configuration_with_model(&model),
            limits: crate::inference_budget::binding::limits(&task.spec.envelope).unwrap(),
        };
        ledger = ledger.register_task(authority.clone()).unwrap().next;
        task.status = Some(KarsTaskStatus {
            phase: Some("Ready".into()),
            observed_generation: task.metadata.generation,
            envelope_digest: Some(authority.authorization_digest.clone()),
            conditions: Some(vec![
                serde_json::from_value(json!({
                    "type":"Ready","status":"True","reason":"Fixture","message":"Fixture",
                    "lastTransitionTime":"2026-09-08T00:00:00Z"
                }))
                .unwrap(),
            ]),
            inference_budget: Some(TaskBudgetBinding {
                scope: BudgetScope::GovernedInference,
                account: account_ref.clone(),
                root: root.clone(),
                task_uid: task.uid().unwrap(),
                parent_task_uid: parent_uid,
                root_task_uid: root_uid.clone(),
                authorization_digest: authority.authorization_digest,
            }),
            ..Default::default()
        });
        state
            .tasks
            .insert(name, serde_json::to_value(task).unwrap());
    }
    state.team["status"]["inferenceBudgetAccount"] = json!(account_ref);
    let mut account = KarsBudgetAccount::new(
        &account_ref.name,
        KarsBudgetAccountSpec {
            scope: BudgetScope::GovernedInference,
            root,
            limits,
        },
    );
    account.metadata.namespace = Some(apis.namespace.clone());
    account.metadata.uid = Some(account_ref.uid);
    account.metadata.resource_version = Some("1".into());
    account.metadata.labels = Some([(MANAGED_BY.into(), OWNER.into())].into());
    account.metadata.annotations = Some([(BOOTSTRAP.into(), "sealed".into())].into());
    account.status = Some(KarsBudgetAccountStatus {
        ledger: Some(ledger),
    });
    *apis.account.lock().unwrap() = Some(account);
}

#[tokio::test]
async fn governed_team_admission_interleavings_preserve_uids_funding_and_launch_intent() {
    let _lock = ENV_LOCK.lock().await;
    let _environment = BudgetEnvironment::enabled();
    let mut team = team();
    team.spec.envelope.budget.as_mut().unwrap().scope = Some(BudgetScope::GovernedInference);
    team.spec.envelope.budget.as_mut().unwrap().tokens = Some(30);
    team.spec.blueprint = Some(TaskBlueprint {
        model: Some(TaskModel {
            provider: "azure-openai".into(),
            deployment: "reviewed-model".into(),
        }),
        ..Default::default()
    });
    team.spec.roster = vec![TeamRole {
        name: "worker".into(),
        ..Default::default()
    }];
    team.spec.cadence = Some(TeamCadence {
        every_minutes: Some(1),
        ..Default::default()
    });
    let (server, client, store) = setup(&team).await;
    let apis = budget_apis(&server).await;

    cycle(&client, &store).await; // Principal created but not enrolled yet.
    let initial = ids(&store);
    assert_eq!(initial.len(), 2);
    cycle(&client, &store).await;
    assert_eq!(
        ids(&store),
        initial,
        "Pending enrollment must not delete/recreate seats"
    );
    assert!(apis.account.lock().unwrap().is_none());
    enroll(&store, &apis);
    for task in store.lock().unwrap().tasks.values_mut() {
        task["spec"]["execution"] = json!({"launch":true});
    }
    cycle(&client, &store).await;
    {
        let state = store.lock().unwrap();
        assert_eq!(
            state.tasks.len(),
            3,
            "Qualified cadence creates exactly one run"
        );
        assert!(
            state
                .tasks
                .values()
                .all(|task| task["spec"]["execution"]["launch"] == true)
        );
    }
    enroll(&store, &apis);
    let stable_ids = ids(&store);
    let run: KarsTask = {
        let state = store.lock().unwrap();
        serde_json::from_value(
            state
                .tasks
                .values()
                .find(|task| task["metadata"]["annotations"][ANNOT_TEAM_ROLE] == "taskforce")
                .unwrap()
                .clone(),
        )
        .unwrap()
    };
    let identity = ExecutionIdentity {
        task_uid: run.uid().unwrap(),
        authorization_digest: run.envelope_digest(),
        sandbox: ResourceIdentity {
            namespace: "tenant-a".into(),
            name: run.name_any(),
            uid: "sandbox-uid".into(),
        },
        runtime_namespace_uid: "runtime-uid".into(),
        pod_name: "pod".into(),
        pod_uid: "pod-uid".into(),
    };
    let (_, quote) = contract()
        .normalize(
            br#"{"model":"reviewed-model","messages":[{"role":"user","content":"fixture"}]}"#,
            100,
            true,
        )
        .unwrap();
    let request = ReserveRequest {
        account_uid: "account-uid".into(),
        identity: identity.clone(),
        sequence: 1,
        wire_digest: format!("sha256:{}", "a".repeat(64)),
        quote,
    };
    let command = AttemptCommand {
        account_uid: "account-uid".into(),
        identity,
        key: AttemptKey {
            pod_uid: "pod-uid".into(),
            sequence: 1,
        },
        wire_digest: request.wire_digest.clone(),
    };
    {
        let mut account = apis.account.lock().unwrap();
        let ledger = account
            .as_mut()
            .unwrap()
            .status
            .as_mut()
            .unwrap()
            .ledger
            .as_mut()
            .unwrap();
        *ledger = ledger
            .register_session(request.identity.clone())
            .unwrap()
            .next;
        *ledger = ledger.reserve(&request, 100).unwrap().next;
        *ledger = ledger.begin_dispatch(&command, 101).unwrap().next;
    }
    store.lock().unwrap().team["status"]["lastRunAt"] =
        json!((Utc::now() - chrono::Duration::minutes(2)).to_rfc3339());
    for (catalog_failure, store_failure) in
        [(false, false), (true, false), (false, true), (false, false)]
    {
        apis.catalog_failed.store(catalog_failure, Ordering::SeqCst);
        apis.store_failed.store(store_failure, Ordering::SeqCst);
        cycle(&client, &store).await;
        assert_eq!(ids(&store), stable_ids);
        assert!(
            store
                .lock()
                .unwrap()
                .tasks
                .values()
                .all(|task| task["spec"]["execution"]["launch"] == true)
        );
        if !store_failure {
            assert!(matches!(
                crate::inference_budget::launch::admit_new(&client, &run).await,
                Err(crate::inference_budget::store::StoreError::Ledger(
                    crate::inference_budget_contract::BudgetError::Exhausted
                ))
            ));
        }
        let account = apis.account.lock().unwrap();
        let ledger = account
            .as_ref()
            .unwrap()
            .status
            .as_ref()
            .unwrap()
            .ledger
            .as_ref()
            .unwrap();
        assert_eq!(ledger.nodes.len(), 3);
        assert_eq!(ledger.meters.reserved.tokens, 30);
        assert_eq!(ledger.meters.uncertain.tokens, 0);
        assert!(matches!(
            crate::inference_budget::launch::capacity(ledger, &run.uid().unwrap()),
            Err(crate::inference_budget_contract::BudgetError::Exhausted)
        ));
    }
    {
        let mut account = apis.account.lock().unwrap();
        let ledger = account
            .as_mut()
            .unwrap()
            .status
            .as_mut()
            .unwrap()
            .ledger
            .as_mut()
            .unwrap();
        *ledger = ledger
            .settle(&Settlement {
                attempt: command,
                usage: Some(Usage {
                    input_tokens: 3,
                    output_tokens: 5,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    reasoning_output_tokens: 0,
                }),
            })
            .unwrap()
            .next;
        assert_eq!(ledger.meters.settled.tokens, 8);
        crate::inference_budget::launch::capacity(ledger, &run.uid().unwrap()).unwrap();
    }
    cycle(&client, &store).await; // Admission resumes, not a re-created principal/run.
    assert_eq!(store.lock().unwrap().tasks.len(), 4);
    for (name, uid) in stable_ids {
        assert_eq!(ids(&store)[&name], uid);
    }
    store.lock().unwrap().team["spec"]["paused"] = json!(true);
    let pause_ids = ids(&store);
    cycle(&client, &store).await;
    assert_eq!(ids(&store), pause_ids);
    assert!(
        store
            .lock()
            .unwrap()
            .tasks
            .values()
            .all(|task| task["spec"]["execution"]["launch"] != true)
    );
    store.lock().unwrap().team["spec"]["roster"] = json!([]);
    cycle(&client, &store).await;
    assert!(
        !store.lock().unwrap().tasks.contains_key("eng-worker"),
        "Actual removed-seat authority must still retire"
    );
}
