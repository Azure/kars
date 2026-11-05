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

#[test]
fn scheduling_pause_preserves_inflight_usage_but_authority_edits_still_revoke() {
    use crate::inference_budget::binding::bind_authority;
    let now = 100;
    let account = original_account(now);
    let ledger = Store::ledger(&account).unwrap();
    let (_, authority) = task_authority("A");
    let paused = bind_authority(ledger, &authority, true, true).unwrap().next;
    assert_eq!(&paused, ledger);
    assert!(paused.nodes["root"].active);
    assert!(!paused.sessions["old"].closed);
    assert!(
        !bind_authority(&paused, &authority, true, true)
            .unwrap()
            .changed
    );
    let (_, updated) = task_authority("B");
    let edited = bind_authority(&paused, &updated, true, true).unwrap().next;
    assert!(!edited.nodes["root"].active);
    assert_eq!(edited.nodes["root"].authority, updated);
    assert!(edited.sessions["old"].closed);
    assert!(edited.meters.uncertain.tokens > 0);
    let resumed = bind_authority(&edited, &updated, false, true).unwrap().next;
    assert!(resumed.nodes["root"].active);
    assert_eq!(resumed.meters, edited.meters);
    assert!(resumed.sessions["old"].closed);
    assert!(resumed.register_session(identity("old")).is_err());
    assert!(
        !bind_authority(&resumed, &updated, false, true)
            .unwrap()
            .changed
    );
}

#[test]
fn paused_scheduling_allows_new_phases_only_under_a_running_branch() {
    use crate::inference_budget::binding::{bind_authority, team_branch_is_running};
    let (mut task, parent) = task_authority("A");
    task.annotations_mut()
        .insert("kars.azure.com/team-role".into(), "taskforce".into());
    let account = original_account(100);
    let ledger = Store::ledger(&account).unwrap();
    assert!(team_branch_is_running(&[task.clone()]));
    let mut child = parent.clone();
    child.task.name = "new-phase".into();
    child.task.uid = "new-phase".into();
    child.parent_uid = Some("root".into());
    let next = bind_authority(
        ledger,
        &child,
        !team_branch_is_running(&[task.clone()]),
        true,
    )
    .unwrap()
    .next;
    assert!(next.nodes["new-phase"].active);
    assert_eq!(next.meters, ledger.meters);
    assert_eq!(next.sessions, ledger.sessions);
    let mut role_parent = task.clone();
    role_parent
        .annotations_mut()
        .remove("kars.azure.com/team-role");
    role_parent.spec.execution.as_mut().unwrap().launch = false;
    assert!(team_branch_is_running(&[task.clone(), role_parent.clone()]));
    role_parent
        .annotations_mut()
        .insert("kars.azure.com/halted".into(), "true".into());
    assert!(!team_branch_is_running(&[task.clone(), role_parent]));
    task.spec.execution.as_mut().unwrap().launch = false;
    assert!(!team_branch_is_running(&[task.clone()]));
    task.spec.execution.as_mut().unwrap().launch = true;
    task.annotations_mut()
        .insert("kars.azure.com/halted".into(), "true".into());
    assert!(!team_branch_is_running(&[task.clone()]));
    task.annotations_mut().remove("kars.azure.com/halted");
    task.annotations_mut().remove("kars.azure.com/team-role");
    assert!(!team_branch_is_running(&[task]));
}

#[test]
fn paused_team_binding_can_prepare_new_members_without_reopening_parent() {
    use crate::inference_budget::binding::bind_authority;
    let (_, parent) = task_authority("A");
    let empty = Ledger::new("account-uid".into(), root(), parent.limits).unwrap();
    let dormant = bind_authority(&empty, &parent, true, true).unwrap().next;
    let mut child = parent.clone();
    child.task.name = "child".into();
    child.task.uid = "child".into();
    child.parent_uid = Some("root".into());
    let dormant = bind_authority(&dormant, &child, true, true).unwrap().next;
    assert!(dormant.nodes.values().all(|node| !node.active));
    assert!(
        !bind_authority(&dormant, &child, true, true)
            .unwrap()
            .changed
    );
    assert!(bind_authority(&dormant, &child, false, true).is_err());
    let resumed = bind_authority(&dormant, &parent, false, true).unwrap().next;
    let resumed = bind_authority(&resumed, &child, false, true).unwrap().next;
    assert!(resumed.nodes.values().all(|node| node.active));
    assert_eq!(resumed.meters, dormant.meters);
}

#[test]
fn team_resume_keeps_stopped_branches_dormant_without_account_writes() {
    use crate::inference_budget::binding::{bind_authority, team_branch_can_resume};
    let account = original_account(100);
    let ledger = Store::ledger(&account).unwrap();
    let (mut task, authority) = task_authority("A");
    let paused = ledger.close_subtree(&authority.task.uid).unwrap().next;
    task.spec.execution.as_mut().unwrap().launch = false;
    let can_resume = team_branch_can_resume(&[task.clone()]);
    assert!(!can_resume);
    for _ in 0..16 {
        let unchanged = bind_authority(&paused, &authority, false, can_resume).unwrap();
        assert!(!unchanged.changed);
        assert_eq!(unchanged.next, paused);
    }
    let (mut child_task, mut child) = task_authority("A");
    child_task.metadata.name = Some("child".into());
    child.task.name = "child".into();
    child.task.uid = "child".into();
    child.parent_uid = Some("root".into());
    let dormant = bind_authority(&paused, &child, true, true).unwrap().next;
    assert!(!team_branch_can_resume(&[task.clone(), child_task]));
    assert!(
        !bind_authority(&dormant, &child, false, false)
            .unwrap()
            .changed
    );
    let (_, updated) = task_authority("B");
    let edited = bind_authority(&paused, &updated, false, false)
        .unwrap()
        .next;
    assert!(!edited.nodes["root"].active);
    assert_eq!(edited.meters, paused.meters);
    assert_eq!(edited.sessions, paused.sessions);
    task.spec.execution.as_mut().unwrap().launch = true;
    assert!(team_branch_can_resume(&[task]));
    assert!(
        bind_authority(&edited, &updated, false, true)
            .unwrap()
            .next
            .nodes["root"]
            .active
    );
}

#[test]
fn team_reactivation_preserves_prelaunch_enrollment_and_halt_fences() {
    use crate::inference_budget::binding::{bind_authority, team_branch_can_resume};
    let (mut task, authority) = task_authority("A");
    let empty = Ledger::new("account-uid".into(), root(), authority.limits).unwrap();
    task.spec.execution.as_mut().unwrap().launch = false;
    let prepared = bind_authority(&empty, &authority, false, false)
        .unwrap()
        .next;
    assert!(prepared.nodes["root"].active);
    assert!(
        !bind_authority(&prepared, &authority, false, false)
            .unwrap()
            .changed
    );
    task.spec.execution = None;
    assert!(team_branch_can_resume(&[task.clone()]));
    task.annotations_mut()
        .insert("kars.azure.com/halted".into(), "operator stop".into());
    assert!(!team_branch_can_resume(&[task.clone()]));
    task.spec.execution = Some(TaskExecution {
        launch: true,
        runtime: None,
    });
    assert!(!team_branch_can_resume(&[task]));
}

#[tokio::test]
async fn paused_team_recovery_keeps_live_dispatch_but_revokes_deleted_team() {
    let now = chrono::Utc::now().timestamp();
    let mut original = original_account(now);
    original.spec.root.kind = RootKind::KarsTeam;
    original
        .status
        .as_mut()
        .unwrap()
        .ledger
        .as_mut()
        .unwrap()
        .root
        .kind = RootKind::KarsTeam;
    original.status = Some(project(&original, None));
    let expected = Store::ledger(&original).unwrap().clone();
    let (server, store, _) = setup(Some(original.clone()), Fault::None).await;
    let (task, _) = task_authority("A");
    live_sources(&server, Arc::new(Mutex::new(task.clone())), None).await;
    let team: crate::kars_team::KarsTeam = serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTeam",
        "metadata":{"name":"root","namespace":"workspace","uid":"root","resourceVersion":"1"},
        "spec":{"charter":"Finish admitted work", "envelope":task.spec.envelope, "paused":true}
    }))
    .unwrap();
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsteams/root",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(team))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/api/v1/namespaces/kars-root/pods/pod-old",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"v1", "kind":"Pod",
            "metadata":{"name":"pod-old","namespace":"kars-root","uid":"old"}
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    let client = test_client(&server);
    reconcile_account(&client, &store, &original).await.unwrap();
    let current = store
        .read(&original.spec.root, "account-uid")
        .await
        .unwrap();
    assert_eq!(Store::ledger(&current).unwrap(), &expected);
    assert!(!expected.sessions["old"].closed);
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karsteams/root",
        ))
        .respond_with(failure(404))
        .with_priority(1)
        .mount(&server)
        .await;
    reconcile_account(&client, &store, &current).await.unwrap();
    let closed = store
        .read(&original.spec.root, "account-uid")
        .await
        .unwrap();
    assert_eq!(
        Store::ledger(&closed).unwrap(),
        &expected.close_account().unwrap().next
    );
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
    fn respond(&self, request: &Request) -> ResponseTemplate {
        // Root-lifetime GET precedes recovery's ledger snapshot; the per-node
        // authority GET follows it. Interleave deterministically at the latter.
        if self.reads.fetch_add(1, Ordering::SeqCst) == 1
            && let Some(enrollment) = &self.enrollment
        {
            enrollment.install();
        }
        let task = self.task.lock().unwrap();
        if request.url.path().ends_with("/karstasks") {
            ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList",
                "metadata":{"resourceVersion":"123"}, "items":[&*task]
            }))
        } else {
            ResponseTemplate::new(200).set_body_json(&*task)
        }
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
    for suffix in ["/root", ""] {
        Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(format!(
                "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks{suffix}",
            )))
            .respond_with(LiveTask {
                task: task.clone(),
                reads: reads.clone(),
                enrollment: enrollment.clone(),
            })
            .with_priority(1)
            .mount(server)
            .await;
    }
    reads
}

async fn changed_authority_survives(conflict: bool, batched: bool) {
    let now = chrono::Utc::now().timestamp();
    let mut original = original_account(now);
    if batched {
        let (_, mut child) = task_authority("A");
        child.task.name = "child".into();
        child.task.uid = "child".into();
        child.parent_uid = Some("root".into());
        original.status.as_mut().unwrap().ledger = Some(
            Store::ledger(&original)
                .unwrap()
                .register_task(child)
                .unwrap()
                .next,
        );
        original.status = Some(project(&original, None));
    }
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
    let client = test_client(&server);
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
    changed_authority_survives(false, false).await;
}

#[tokio::test]
async fn recovery_authority_change_on_cas_retry_preserves_new_session_and_inflight_work() {
    changed_authority_survives(true, false).await;
}

#[tokio::test]
async fn recovery_batched_snapshot_preserves_concurrent_enrollment_and_revokes_changed_authority() {
    changed_authority_survives(false, true).await;
}

#[tokio::test]
async fn recovery_batched_snapshot_cas_retry_preserves_new_inflight_work() {
    changed_authority_survives(true, true).await;
}

const TASKS: &str = "/apis/kars.azure.com/v1alpha1/namespaces/workspace/karstasks";

fn named_task(name: &str) -> KarsTask {
    let mut task = task_authority("A").0;
    task.metadata.name = Some(name.into());
    task.metadata.uid = Some(name.into());
    task
}

#[tokio::test]
async fn recovery_snapshot_batches_128_tasks_and_rechecks_next_scan() {
    use crate::inference_budget::recovery::task_snapshot;
    use wiremock::matchers::{method, path, query_param};
    let server = MockServer::start().await;
    let client = test_client(&server);
    let api = Api::namespaced(client, "workspace");
    let tasks: Vec<_> = (0..128).map(|i| named_task(&format!("task-{i}"))).collect();
    let names: std::collections::BTreeSet<String> =
        tasks.iter().map(ResourceExt::name_any).collect();
    Mock::given(method("GET"))
        .and(path(TASKS))
        .and(query_param("limit", "128"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList",
            "metadata":{"resourceVersion":"123"}, "items":tasks
        })))
        .expect(2)
        .mount(&server)
        .await;
    for _ in 0..2 {
        let snapshot = task_snapshot(&api, names.clone()).await.unwrap();
        assert_eq!(snapshot.len(), 128);
        assert!(snapshot.values().all(Option::is_some));
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn recovery_snapshot_follows_continuation_before_proving_absence() {
    use crate::inference_budget::recovery::task_snapshot;
    use wiremock::matchers::{method, path, query_param};
    let server = MockServer::start().await;
    let client = test_client(&server);
    let api = Api::namespaced(client, "workspace");
    Mock::given(method("GET"))
        .and(path(TASKS))
        .and(query_param("continue", "second"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList",
            "metadata":{"resourceVersion":"123"}, "items":[named_task("child")]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(TASKS))
        .and(query_param("limit", "128"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList",
            "metadata":{"resourceVersion":"123", "continue":"second"},
            "items":[named_task("root"), named_task("unrelated")]
        })))
        .with_priority(2)
        .expect(1)
        .mount(&server)
        .await;
    let snapshot = task_snapshot(
        &api,
        ["root".into(), "child".into(), "absent".into()].into(),
    )
    .await
    .unwrap();
    assert_eq!(snapshot.len(), 3);
    assert_eq!(
        snapshot["root"].as_ref().unwrap().uid().as_deref(),
        Some("root")
    );
    assert_eq!(
        snapshot["child"].as_ref().unwrap().uid().as_deref(),
        Some("child")
    );
    assert!(snapshot["absent"].is_none());
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn recovery_snapshot_never_interprets_truncated_inventory_as_absence() {
    use crate::inference_budget::recovery::task_snapshot;
    use wiremock::matchers::{method, path};
    let server = MockServer::start().await;
    let client = test_client(&server);
    let api = Api::namespaced(client, "workspace");
    let page = Arc::new(AtomicUsize::new(0));
    let seen = page.clone();
    Mock::given(method("GET"))
        .and(path(TASKS))
        .respond_with(move |_: &Request| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList",
                "metadata":{"resourceVersion":"123", "continue":format!("page-{}",n+1)},
                "items": if n == 0 { vec![named_task("root")] } else { vec![] }
            }))
        })
        .expect(4)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{TASKS}/child")))
        .respond_with(ResponseTemplate::new(200).set_body_json(named_task("child")))
        .expect(1)
        .mount(&server)
        .await;
    let snapshot = task_snapshot(&api, ["root".into(), "child".into()].into())
        .await
        .unwrap();
    assert!(snapshot.values().all(Option::is_some));
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn recovery_snapshot_absence_requires_a_complete_inventory() {
    use crate::inference_budget::recovery::task_snapshot;
    let server = MockServer::start().await;
    let client = test_client(&server);
    let api = Api::namespaced(client, "workspace");
    Mock::given(wiremock::matchers::path(TASKS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList",
            "metadata":{"resourceVersion":"123"}, "items":[named_task("root")]
        })))
        .mount(&server)
        .await;
    let snapshot = task_snapshot(&api, ["root".into(), "absent".into()].into())
        .await
        .unwrap();
    assert!(snapshot["root"].is_some());
    assert!(snapshot["absent"].is_none());
}

#[tokio::test]
async fn recovery_snapshot_fails_closed_on_list_error_or_inconsistent_pagination() {
    use crate::inference_budget::recovery::task_snapshot;
    for fault in [
        "forbidden",
        "changed-version",
        "repeated-token",
        "duplicate-task",
        "missing-version",
        "oversized-page",
    ] {
        let server = MockServer::start().await;
        let client = test_client(&server);
        let api = Api::namespaced(client, "workspace");
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(wiremock::matchers::path(TASKS))
            .respond_with(move |_: &Request| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if fault == "forbidden" {
                    return failure(403);
                }
                let version = if fault == "missing-version" {
                    None
                } else if fault == "changed-version" && n > 0 {
                    Some("124")
                } else {
                    Some("123")
                };
                let items = match fault {
                    "duplicate-task" => vec![named_task("root")],
                    "oversized-page" => (0..129)
                        .map(|index| named_task(&format!("task-{index}")))
                        .collect(),
                    _ => vec![],
                };
                ResponseTemplate::new(200).set_body_json(json!({
                    "apiVersion":"kars.azure.com/v1alpha1", "kind":"KarsTaskList",
                    "metadata":{"resourceVersion":version,
                        "continue":if fault == "repeated-token" {"same".into()} else {format!("page-{n}")}},
                    "items": items
                }))
            }).mount(&server).await;
        assert!(
            task_snapshot(&api, ["root".into(), "child".into()].into())
                .await
                .is_err(),
            "{fault}"
        );
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.url.path() == TASKS)
        );
    }
}

#[tokio::test]
async fn recovery_unchanged_authority_with_invalid_live_source_still_revokes_and_funds_old_work() {
    let now = chrono::Utc::now().timestamp();
    let original = original_account(now);
    let (server, store, state) = setup(Some(original.clone()), Fault::None).await;
    live_sources(&server, Arc::new(Mutex::new(task_authority("B").0)), None).await;
    let client = test_client(&server);
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
