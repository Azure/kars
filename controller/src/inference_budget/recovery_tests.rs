// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{
    inference_budget::{recovery::reconcile_account, status::project},
    inference_budget_contract::AttemptPhase,
    kars_task::{
        KarsTask, KarsTaskSpec, TaskBlueprint, TaskBudget, TaskEnvelope, TaskExecution, TaskModel,
    },
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn task_authority(revision: &str) -> (KarsTask, TaskAuthority) {
    let model = TaskModel {
        provider: "azure-openai".into(),
        deployment: "fixture".into(),
    };
    let mut task = KarsTask::new(
        "root",
        KarsTaskSpec {
            objective: "recovery interleaving".into(),
            envelope: TaskEnvelope {
                tier: 3,
                authority_ceiling: 3,
                delegation_depth: 2,
                budget: Some(TaskBudget {
                    scope: Some(BudgetScope::GovernedInference),
                    tokens: Some(100),
                    usd_micros: Some(20),
                }),
                ..Default::default()
            },
            execution: Some(TaskExecution {
                launch: true,
                runtime: None,
            }),
            blueprint: Some(TaskBlueprint {
                model: Some(model.clone()),
                instructions: Some(revision.into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    task.metadata.namespace = Some("workspace".into());
    task.metadata.uid = Some("root".into());
    task.metadata.resource_version = Some(revision.into());
    let authority = TaskAuthority {
        task: root().resource,
        parent_uid: None,
        root_task_uid: "root".into(),
        authorization_digest: task.envelope_digest(),
        effective_authorization: task.spec.authorization_configuration_with_model(&model),
        limits: Limits {
            tokens: Some(100),
            usd_micros: Some(20),
        },
    };
    (task, authority)
}

fn inflight(ledger: Ledger, authority: &TaskAuthority, pod: &str, now: i64) -> Ledger {
    let mut request = reserve(pod);
    request.identity.authorization_digest = authority.authorization_digest.clone();
    request.identity.sandbox.name = authority.task.name.clone();
    let command = AttemptCommand {
        account_uid: request.account_uid.clone(),
        key: AttemptKey {
            pod_uid: pod.into(),
            sequence: request.sequence,
        },
        identity: request.identity.clone(),
        wire_digest: request.wire_digest.clone(),
    };
    ledger
        .register_session(request.identity.clone())
        .unwrap()
        .next
        .reserve(&request, now)
        .unwrap()
        .next
        .begin_dispatch(&command, now)
        .unwrap()
        .next
}

fn original_account(now: i64) -> KarsBudgetAccount {
    let (_, authority) = task_authority("A");
    let mut original = account();
    original.spec.limits = authority.limits;
    let ledger = Ledger::new("account-uid".into(), root(), authority.limits)
        .unwrap()
        .register_task(authority.clone())
        .unwrap()
        .next;
    original.status.as_mut().unwrap().ledger = Some(inflight(ledger, &authority, "old", now));
    original.status = Some(project(&original, None));
    original
}

fn enroll_new_authority(ledger: &Ledger, authority: &TaskAuthority, now: i64) -> Ledger {
    // These are ensure_task's close/update/resume transitions, followed by a
    // genuinely new Pod session and accepted work under the refreshed authority.
    let next = ledger
        .close_subtree(&authority.task.uid)
        .unwrap()
        .next
        .update_authority(authority.clone())
        .unwrap()
        .next
        .resume_task(&authority.task.uid, &authority.authorization_digest)
        .unwrap()
        .next;
    inflight(next, authority, "new", now)
}

#[derive(Clone)]
struct Enrollment {
    state: Arc<Mutex<State>>,
    authority: TaskAuthority,
    now: i64,
    occurred: Arc<AtomicBool>,
}

impl Enrollment {
    fn install(&self) {
        assert!(!self.occurred.swap(true, Ordering::SeqCst));
        let mut state = self.state.lock().unwrap();
        let next = enroll_new_authority(
            Store::ledger(state.account.as_ref().unwrap()).unwrap(),
            &self.authority,
            self.now,
        );
        state.version += 1;
        state.writes += 1;
        let version = state.version.to_string();
        let account = state.account.as_mut().unwrap();
        account.metadata.resource_version = Some(version);
        account.status.as_mut().unwrap().ledger = Some(next);
        account.status = Some(project(account, None));
    }
}

struct LiveTask {
    task: Arc<Mutex<KarsTask>>,
    reads: Arc<AtomicUsize>,
    enrollment: Option<Enrollment>,
}

impl Respond for LiveTask {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        // Root-lifetime GET precedes recovery's ledger snapshot; the per-node
        // authority GET follows it. Interleave deterministically at the latter.
        if self.reads.fetch_add(1, Ordering::SeqCst) == 1
            && let Some(enrollment) = &self.enrollment
        {
            enrollment.install();
        }
        ResponseTemplate::new(200).set_body_json(&*self.task.lock().unwrap())
    }
}

struct Conflict(Enrollment);

impl Respond for Conflict {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let incoming: KarsBudgetAccount = serde_json::from_slice(&request.body).unwrap();
        if !self.0.occurred.load(Ordering::SeqCst)
            && !Store::ledger(&incoming).unwrap().nodes["root"].active
        {
            {
                let state = self.0.state.lock().unwrap();
                let current = state.account.as_ref().unwrap();
                assert_eq!(incoming.metadata.uid, current.metadata.uid);
                assert_eq!(
                    incoming.metadata.resource_version,
                    current.metadata.resource_version
                );
            }
            self.0.install();
            return failure(409);
        }
        Server(self.0.state.clone()).respond(request)
    }
}

async fn live_sources(
    server: &MockServer,
    task: Arc<Mutex<KarsTask>>,
    enrollment: Option<Enrollment>,
) -> Arc<AtomicUsize> {
    let reads = Arc::new(AtomicUsize::new(0));
    for (path, object) in [
        (
            "/api/v1/namespaces/workspace",
            json!({"apiVersion":"v1", "kind":"Namespace",
                "metadata":{"name":"workspace", "uid":"workspace-uid"}}),
        ),
        (
            "/api/v1/namespaces/kars-root/pods/pod-new",
            json!({"apiVersion":"v1", "kind":"Pod",
                "metadata":{"name":"pod-new", "namespace":"kars-root", "uid":"new"}}),
        ),
    ] {
        Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(path))
            .respond_with(ResponseTemplate::new(200).set_body_json(object))
            .with_priority(1)
            .mount(server)
            .await;
    }
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks/root",
        ))
        .respond_with(LiveTask {
            task,
            reads: reads.clone(),
            enrollment,
        })
        .with_priority(1)
        .mount(server)
        .await;
    reads
}

async fn changed_authority_survives(conflict: bool) {
    let now = chrono::Utc::now().timestamp();
    let original = original_account(now);
    let (task, authority) = task_authority("B");
    let expected = enroll_new_authority(Store::ledger(&original).unwrap(), &authority, now);
    let (server, store, state) = setup(Some(original.clone()), Fault::None).await;
    let enrollment = Enrollment {
        state: state.clone(),
        authority,
        now,
        occurred: Arc::new(AtomicBool::new(false)),
    };
    let live = Arc::new(Mutex::new(task));
    let reads = live_sources(
        &server,
        live.clone(),
        (!conflict).then_some(enrollment.clone()),
    )
    .await;
    if conflict {
        Mock::given(wiremock::matchers::method("PUT"))
            .and(wiremock::matchers::path(format!("{OBJECT}/status")))
            .respond_with(Conflict(enrollment.clone()))
            .with_priority(1)
            .mount(&server)
            .await;
    }
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    reconcile_account(&client, &store, &original).await.unwrap();
    assert!(enrollment.occurred.load(Ordering::SeqCst));
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    let current = store.read(&root(), "account-uid").await.unwrap();
    assert_eq!(Store::ledger(&current).unwrap(), &expected);
    assert!(expected.nodes["root"].active);
    assert!(expected.sessions["old"].closed);
    assert!(!expected.sessions["new"].closed);
    assert_eq!(
        expected.meters.uncertain,
        Amounts {
            tokens: 30,
            usd_micros: 5
        }
    );
    assert_eq!(
        expected.meters.reserved,
        Amounts {
            tokens: 30,
            usd_micros: 5
        }
    );
    for (pod, phase) in [
        ("old", AttemptPhase::Uncertain),
        ("new", AttemptPhase::InFlight),
    ] {
        let key = AttemptKey {
            pod_uid: pod.into(),
            sequence: 1,
        }
        .storage_key();
        assert_eq!(expected.attempts[&key].phase, phase);
    }

    // Deferral must actually re-read B's live source on the next scan, and must
    // still revoke B if that source becomes invalid without another enrollment.
    reconcile_account(&client, &store, &current).await.unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 4);
    let current = store.read(&root(), "account-uid").await.unwrap();
    assert_eq!(Store::ledger(&current).unwrap(), &expected);
    *live.lock().unwrap() = task_authority("C").0;
    reconcile_account(&client, &store, &current).await.unwrap();
    assert_eq!(reads.load(Ordering::SeqCst), 6);
    let closed = store.read(&root(), "account-uid").await.unwrap();
    let ledger = Store::ledger(&closed).unwrap();
    assert!(!ledger.nodes["root"].active);
    assert!(ledger.sessions.values().all(|session| session.closed));
    assert_eq!(
        ledger.meters.total().unwrap(),
        expected.meters.total().unwrap()
    );
    assert_eq!(ledger.meters.reserved, Amounts::default());
    assert_eq!(
        ledger.meters.uncertain,
        Amounts {
            tokens: 60,
            usd_micros: 10
        }
    );
    assert_eq!(ledger.limits, expected.limits);
    assert_eq!(closed.metadata.uid, original.metadata.uid);
    assert_ne!(
        closed.metadata.resource_version,
        original.metadata.resource_version
    );
}

#[tokio::test]
async fn recovery_authority_change_before_transaction_preserves_new_session_and_inflight_work() {
    changed_authority_survives(false).await;
}

#[tokio::test]
async fn recovery_authority_change_on_cas_retry_preserves_new_session_and_inflight_work() {
    changed_authority_survives(true).await;
}

#[tokio::test]
async fn recovery_unchanged_authority_with_invalid_live_source_still_revokes_and_funds_old_work() {
    let now = chrono::Utc::now().timestamp();
    let original = original_account(now);
    let (server, store, state) = setup(Some(original.clone()), Fault::None).await;
    live_sources(&server, Arc::new(Mutex::new(task_authority("B").0)), None).await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    reconcile_account(&client, &store, &original).await.unwrap();
    let current = state.lock().unwrap().account.clone().unwrap();
    let ledger = Store::ledger(&current).unwrap();
    assert_eq!(
        ledger,
        &Store::ledger(&original)
            .unwrap()
            .close_subtree("root")
            .unwrap()
            .next
    );
    assert!(!ledger.nodes["root"].active);
    assert!(ledger.sessions["old"].closed);
    assert_eq!(
        ledger.meters.uncertain,
        Amounts {
            tokens: 30,
            usd_micros: 5
        }
    );
    assert_eq!(ledger.meters.reserved, Amounts::default());
    assert_eq!(current.metadata.uid, original.metadata.uid);
}
