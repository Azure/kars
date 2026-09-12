// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn stored(f: &fixture::Fixture) -> evidence::Evidence {
    let store = f.store.lock().unwrap();
    serde_json::from_str(
        store.cms[&evidence::name(&f.eval)]["data"]["evidence.json"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

fn not_ready(status: &KarsEvalStatus) {
    assert_ne!(status.phase.as_deref(), Some("Ready"));
    assert_eq!(
        conditions::find(status.conditions.as_ref().unwrap(), "Ready")
            .unwrap()
            .status,
        "False"
    );
}

async fn completed_request() -> fixture::Fixture {
    let mut f = fixture::setup().await;
    f.eval.spec.schedule = Some("*/15 * * * *".into());
    let name = f.request().await;
    f.complete(&name);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    f
}

fn gc(f: &fixture::Fixture) {
    let mut store = f.store.lock().unwrap();
    store.jobs.clear();
    store.pods.clear();
}

async fn change_intent(f: &mut fixture::Fixture, change: &str) {
    if change.starts_with("target") {
        let mut store = f.store.lock().unwrap();
        let generation = store.target["metadata"]["generation"].as_i64().unwrap() + 1;
        store.target["metadata"]["generation"] = json!(generation);
        store.target["spec"]["inferenceRef"]["name"] = json!("changed-policy");
        if change == "target-uid" {
            store.target["metadata"]["uid"] = json!("new-target-uid");
        }
    } else {
        f.eval.metadata.generation = Some(f.eval.metadata.generation.unwrap() + 1);
        match change {
            "schedule" => f.eval.spec.schedule = Some("*/5 * * * *".into()),
            "runner" => f.eval.spec.runner_image = Some("changed-runner:latest".into()),
            "corpus" => f.eval.spec.corpus.builtin = Some("banned-tools".into()),
            _ => panic!("unknown intent change"),
        }
    }
    f.corpus = resolve_corpus(&f.eval.spec.corpus).await.unwrap();
    f.refresh_intent().await;
}

fn time(seconds: usize) -> String {
    format!("2026-09-10T20:{:02}:{:02}Z", seconds / 60, seconds % 60)
}

fn scheduled_completion(f: &fixture::Fixture, run: usize) -> String {
    let name = format!("scheduled-transition-{run}");
    let start = run * 10;
    {
        let mut store = f.store.lock().unwrap();
        let cron = store.crons[&cron_job_name("eval")].clone();
        let mut job = cron["spec"]["jobTemplate"].clone();
        job["apiVersion"] = json!("batch/v1");
        job["kind"] = json!("Job");
        job["metadata"]["name"] = json!(name);
        job["metadata"]["namespace"] = json!("tenant");
        job["metadata"]["uid"] = json!(format!("{name}-uid"));
        job["metadata"]["resourceVersion"] = json!("60");
        job["metadata"]["generation"] = json!(1);
        job["metadata"]["creationTimestamp"] = json!(time(start));
        job["metadata"]["ownerReferences"] = json!([{"apiVersion":"batch/v1","kind":"CronJob",
            "name":cron_job_name("eval"),"uid":cron["metadata"]["uid"],"controller":true}]);
        store.jobs.insert(name.clone(), job);
    }
    f.complete(&name);
    let corpus = kars_eval_corpus::parse(&f.corpus.bytes).unwrap();
    let cases: Vec<_> = corpus.cases.iter().map(|case| {
        let mut actual = json!({"decision":case.expect.decision.as_wire(),
            "byPolicyKind":case.expect.by_policy_kind.map(|kind|kind.as_wire())});
        if let Some(required) = case.expect.decision_at_least_some {
            actual["observations"] = json!([{"seq":0,"decision":required.as_wire()}]);
        }
        json!({"caseId":case.id,"durationMs":1,
            "expected":{"decision":case.expect.decision.as_wire(),
                "byPolicyKind":case.expect.by_policy_kind.map(|kind|kind.as_wire()),
                "decisionAtLeastSome":case.expect.decision_at_least_some.map(|decision|decision.as_wire())},
            "actual":actual,"verdict":{"result":"Pass"}})
    }).collect();
    let mut store = f.store.lock().unwrap();
    store.jobs.get_mut(&name).unwrap()["status"]["conditions"][0]["lastTransitionTime"] =
        json!(time(start + 5));
    let pod = store.pods.get_mut(&format!("{name}-pod")).unwrap();
    pod["status"]["containerStatuses"][0]["state"]["terminated"]["startedAt"] = json!(time(start));
    pod["status"]["containerStatuses"][0]["state"]["terminated"]["finishedAt"] =
        json!(time(start + 4));
    store.report = json!({"schemaVersion":"v2","corpusName":corpus.name,"corpusDigest":f.corpus.digest,
        "routerBase":f.intent.router,"startedAt":time(start + 1),"completedAt":time(start + 3),
        "total":cases.len(),"passed":cases.len(),"failed":0,"errored":0,"results":cases});
    name
}

#[tokio::test]
async fn completed_explicit_request_allows_only_a_new_attributed_report_after_intent_change() {
    for change in ["schedule", "runner", "corpus", "target", "target-uid"] {
        for old_job_state in ["live", "collected", "terminating"] {
            let mut f = completed_request().await;
            let old = stored(&f);
            if old_job_state == "collected" {
                gc(&f);
            } else if old_job_state == "terminating" {
                f.store.lock().unwrap().jobs.get_mut(&old.job_name).unwrap()["metadata"]["deletionTimestamp"] =
                    json!(time(6));
            }
            change_intent(&mut f, change).await;
            assert!(!old.current(&f.intent));
            assert!(old.fulfills_request(&f.intent));
            for _ in 0..2 {
                not_ready(&f.reconcile().await);
                assert_eq!(stored(&f), old, "{change}: old report must not be recast");
                assert_eq!(f.store.lock().unwrap().report_writes, 1);
            }
            let name = scheduled_completion(&f, 1);
            let status = f.reconcile().await;
            assert_eq!(
                status.phase.as_deref(),
                Some("Ready"),
                "{change}/{old_job_state}"
            );
            let current = stored(&f);
            assert!(current.current(&f.intent));
            assert_eq!(current.job_name, name);
            assert_ne!(current.job_uid, old.job_uid);
            assert_ne!(current.intent, old.intent);
            assert_eq!(current.eval_generation, f.eval.metadata.generation.unwrap());
            assert_eq!(current.request_marker, old.request_marker);
            assert_eq!(current.request_job, old.request_job);
            assert_eq!(f.store.lock().unwrap().report_writes, 2);
        }
    }
}

#[tokio::test]
async fn fulfillment_survives_repeated_scheduled_intent_transitions_and_receipt_gc() {
    let mut f = completed_request().await;
    let request_token = stored(&f).request_marker;
    for (index, change) in ["schedule", "runner", "target", "corpus"]
        .into_iter()
        .enumerate()
    {
        let prior = stored(&f);
        if index.is_multiple_of(2) {
            gc(&f);
        }
        change_intent(&mut f, change).await;
        not_ready(&f.reconcile().await);
        assert_eq!(stored(&f), prior);
        let name = scheduled_completion(&f, index + 1);
        assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
        let current = stored(&f);
        assert_eq!(current.job_name, name);
        assert!(current.current(&f.intent));
        assert_eq!(current.request_marker, request_token);
        assert_eq!(f.store.lock().unwrap().report_writes, index + 2);
        if index.is_multiple_of(2) {
            gc(&f);
            assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
            assert_eq!(stored(&f), current);
        }
    }
}

#[tokio::test]
async fn missing_outstanding_request_stays_pending_even_with_old_success_and_fresh_schedule() {
    for change in [false, true] {
        let mut f = completed_request().await;
        let old = stored(&f);
        let missing = f.request().await;
        f.store.lock().unwrap().jobs.remove(&missing);
        if change {
            change_intent(&mut f, "runner").await;
        }
        assert!(!old.fulfills_request(&f.intent));
        assert_eq!(f.reconcile().await.phase.as_deref(), Some("Pending"));
        scheduled_completion(&f, 1);
        for _ in 0..2 {
            let status = f.reconcile().await;
            assert_eq!(status.phase.as_deref(), Some("Pending"));
            not_ready(&status);
            assert_eq!(stored(&f), old);
            assert_eq!(f.store.lock().unwrap().report_writes, 1);
        }
    }
}

#[tokio::test]
async fn unauthenticated_historical_receipt_cannot_release_the_request_gate() {
    for missing_auth in ["digest", "uid"] {
        let mut f = completed_request().await;
        let old = stored(&f);
        gc(&f);
        let status = f.eval.status.as_mut().unwrap();
        match missing_auth {
            "digest" => status.report_evidence_digest = None,
            "uid" => status.report_config_map_uid = Some("foreign-receipt".into()),
            _ => unreachable!(),
        }
        change_intent(&mut f, "schedule").await;
        assert_eq!(f.reconcile().await.phase.as_deref(), Some("Pending"));
        scheduled_completion(&f, 1);
        assert_eq!(f.reconcile().await.phase.as_deref(), Some("Pending"));
        assert_eq!(stored(&f), old);
        assert_eq!(f.store.lock().unwrap().report_writes, 1);
    }
}

#[tokio::test]
async fn changed_historical_job_identity_or_token_cannot_release_the_gate() {
    for mutation in ["uid", "generation", "token", "intent", "owner", "terminal"] {
        let mut f = completed_request().await;
        let old = stored(&f);
        change_intent(&mut f, "schedule").await;
        not_ready(&f.reconcile().await);
        scheduled_completion(&f, 1);
        {
            let mut store = f.store.lock().unwrap();
            let job = store.jobs.get_mut(&old.job_name).unwrap();
            match mutation {
                "uid" => job["metadata"]["uid"] = json!("foreign"),
                "generation" => job["metadata"]["generation"] = json!(2),
                "token" => job["metadata"]["annotations"][workloads::RUN_TOKEN] = json!("foreign"),
                "intent" => {
                    job["spec"]["template"]["metadata"]["annotations"][workloads::INTENT] =
                        json!("foreign")
                }
                "owner" => job["metadata"]["ownerReferences"][0]["uid"] = json!("foreign"),
                "terminal" => job["status"] = json!({"active":1}),
                _ => unreachable!(),
            }
        }
        assert_eq!(
            f.reconcile().await.phase.as_deref(),
            Some("Degraded"),
            "{mutation}"
        );
        assert_eq!(stored(&f), old);
        assert_eq!(f.store.lock().unwrap().report_writes, 1);
    }
}

#[tokio::test]
async fn historical_fulfillment_does_not_authorize_wrong_current_scheduled_uid_or_token() {
    for mutation in ["uid", "token"] {
        let mut f = completed_request().await;
        let old = stored(&f);
        change_intent(&mut f, "schedule").await;
        not_ready(&f.reconcile().await);
        let name = scheduled_completion(&f, 1);
        {
            let mut store = f.store.lock().unwrap();
            let job = store.jobs.get_mut(&name).unwrap();
            if mutation == "uid" {
                job["metadata"]["ownerReferences"][0]["uid"] = json!("foreign");
            } else {
                job["metadata"]["annotations"][workloads::LAST_TOKEN] = json!("foreign");
            }
        }
        assert_eq!(
            f.reconcile().await.phase.as_deref(),
            Some("Degraded"),
            "{mutation}"
        );
        assert_eq!(stored(&f), old);
        assert_eq!(f.store.lock().unwrap().report_writes, 1);
    }
}

#[tokio::test]
async fn historical_fulfillment_is_rechecked_after_reading_the_new_report() {
    for mutation in ["historical-job-token", "historical-job-uid"] {
        let mut f = completed_request().await;
        let old = stored(&f);
        change_intent(&mut f, "runner").await;
        not_ready(&f.reconcile().await);
        scheduled_completion(&f, 1);
        f.store.lock().unwrap().mutate_on_log = Some(mutation);
        assert_eq!(
            f.reconcile().await.phase.as_deref(),
            Some("Degraded"),
            "{mutation}"
        );
        assert_eq!(stored(&f), old);
        assert_eq!(f.store.lock().unwrap().report_writes, 1);
    }
}

#[tokio::test]
async fn fulfillment_requires_the_exact_eval_request_and_a_nonfuture_generation() {
    let mut f = completed_request().await;
    let old = stored(&f);
    change_intent(&mut f, "runner").await;
    assert!(old.fulfills_request(&f.intent));
    for mutation in [
        "eval-uid",
        "request-token",
        "request-job",
        "future-generation",
    ] {
        let mut different = old.clone();
        match mutation {
            "eval-uid" => different.eval_uid = "foreign".into(),
            "request-token" => different.request_marker = Some("foreign".into()),
            "request-job" => different.request_job = Some("foreign".into()),
            "future-generation" => different.eval_generation = f.intent.generation + 1,
            _ => unreachable!(),
        }
        assert!(!different.fulfills_request(&f.intent), "{mutation}");
    }
}

#[tokio::test]
async fn terminal_inconclusive_receipt_discharges_only_the_request_not_current_readiness() {
    let mut f = fixture::setup().await;
    f.eval.spec.schedule = Some("*/15 * * * *".into());
    let name = f.request().await;
    f.complete(&name);
    f.store.lock().unwrap().report["schemaVersion"] = json!("unsupported");
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Degraded"));
    let old = stored(&f);
    assert_eq!(old.state(), "Inconclusive");
    change_intent(&mut f, "runner").await;
    assert!(old.fulfills_request(&f.intent));
    not_ready(&f.reconcile().await);
    assert_eq!(stored(&f), old);
    let scheduled = scheduled_completion(&f, 1);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let current = stored(&f);
    assert_eq!(current.job_name, scheduled);
    assert!(current.current(&f.intent));
    assert!(current.report.as_ref().unwrap().ready());
}

#[tokio::test]
async fn completed_request_survives_disabling_and_recreating_its_scheduled_parent() {
    let mut f = completed_request().await;
    let explicit = stored(&f).job_name;
    scheduled_completion(&f, 1);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let prior = stored(&f);
    let old_cron_uid =
        f.store.lock().unwrap().crons[&cron_job_name("eval")]["metadata"]["uid"].clone();
    f.store.lock().unwrap().jobs.remove(&explicit);
    f.eval.spec.schedule = None;
    f.eval.metadata.generation = Some(f.eval.metadata.generation.unwrap() + 1);
    f.refresh_intent().await;
    not_ready(&f.reconcile().await);
    assert!(f.store.lock().unwrap().crons.is_empty());
    assert_eq!(stored(&f), prior);
    change_intent(&mut f, "schedule").await;
    not_ready(&f.reconcile().await);
    assert_ne!(
        f.store.lock().unwrap().crons[&cron_job_name("eval")]["metadata"]["uid"],
        old_cron_uid
    );
    let name = scheduled_completion(&f, 2);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    assert_eq!(stored(&f).job_name, name);
    assert!(stored(&f).current(&f.intent));
}
