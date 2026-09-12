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

fn assert_pending(status: &KarsEvalStatus) {
    assert_eq!(status.phase.as_deref(), Some("Pending"));
    let conditions = status.conditions.as_ref().unwrap();
    assert_eq!(
        conditions::find(conditions, "Ready").unwrap().status,
        "False"
    );
    assert_eq!(
        conditions::find(conditions, "Progressing").unwrap().status,
        "True"
    );
    assert_eq!(
        conditions::find(conditions, TYPE_CONFORMANCE_DRIFT)
            .unwrap()
            .status,
        "False"
    );
}

#[tokio::test]
async fn missing_new_request_never_promotes_an_older_success_without_a_report() {
    for previously_requested in [false, true] {
        let mut f = fixture::setup().await;
        let old = if previously_requested {
            let name = f.request().await;
            f.complete(&name);
            name
        } else {
            f.store.lock().unwrap().jobs.keys().next().unwrap().clone()
        };
        let requested = f.request().await;
        f.store.lock().unwrap().jobs.remove(&requested);
        for _ in 0..2 {
            assert_pending(&f.reconcile().await);
        }
        let store = f.store.lock().unwrap();
        assert!(store.jobs.contains_key(&old));
        assert_eq!(store.report_writes, 0);
        assert_eq!(store.eval_status_writes, 2);
        assert!(f.eval.status.as_ref().unwrap().last_result.is_none());
    }
}

#[tokio::test]
async fn a_previous_request_receipt_cannot_satisfy_a_missing_new_request() {
    let mut f = fixture::setup().await;
    let old = f.request().await;
    f.complete(&old);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let receipt = stored(&f);
    let requested = f.request().await;
    assert_ne!(receipt.request_marker, f.intent.request_marker);
    f.store.lock().unwrap().jobs.remove(&requested);
    let status = f.reconcile().await;
    assert_pending(&status);
    assert_eq!(status.last_result.unwrap().job_name, old);
    assert_eq!(stored(&f), receipt);
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
}

#[tokio::test]
async fn request_name_and_owner_do_not_substitute_for_the_produced_token() {
    for mutation in ["job-token", "template-token", "pod-token"] {
        let mut f = fixture::setup().await;
        let requested = f.request().await;
        f.complete(&requested);
        {
            let mut store = f.store.lock().unwrap();
            match mutation {
                "job-token" => {
                    store.jobs.get_mut(&requested).unwrap()["metadata"]["annotations"]
                        [workloads::RUN_TOKEN] = json!("old-token")
                }
                "template-token" => {
                    store.jobs.get_mut(&requested).unwrap()["spec"]["template"]["metadata"]["annotations"]
                        [workloads::LAST_TOKEN] = json!("old-token")
                }
                "pod-token" => {
                    store.pods.get_mut(&format!("{requested}-pod")).unwrap()["metadata"]["annotations"]
                        [workloads::LAST_TOKEN] = json!("old-token")
                }
                _ => unreachable!(),
            }
        }
        let status = f.reconcile().await;
        assert_ne!(status.phase.as_deref(), Some("Ready"), "{mutation}");
        assert_eq!(f.store.lock().unwrap().report_writes, 0, "{mutation}");
    }
}

#[tokio::test]
async fn requested_latest_report_keeps_the_job_and_template_request_identity() {
    let mut f = fixture::setup().await;
    let old_uid = f.store.lock().unwrap().jobs.values().next().unwrap()["metadata"]["uid"].clone();
    let requested = f.request().await;
    f.complete(&requested);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let record = stored(&f);
    let store = f.store.lock().unwrap();
    let job = &store.jobs[&requested];
    assert_ne!(job["metadata"]["uid"], old_uid);
    assert_eq!(record.job_uid, job["metadata"]["uid"].as_str().unwrap());
    assert_eq!(record.job_name, requested);
    assert_eq!(record.request_job.as_deref(), Some(requested.as_str()));
    assert_eq!(
        record.request_marker.as_deref(),
        job["metadata"]["annotations"][workloads::RUN_TOKEN].as_str()
    );
    assert_eq!(
        record.request_marker.as_deref(),
        job["spec"]["template"]["metadata"]["annotations"][workloads::LAST_TOKEN].as_str()
    );
    assert!(record.current(&f.intent));
}

#[tokio::test]
async fn full_reconciliation_claims_creates_acknowledges_and_consumes_the_same_request() {
    let mut f = fixture::setup().await;
    f.eval
        .annotations_mut()
        .insert(ANNOTATION_RUN_NOW.into(), "true".into());
    f.store.lock().unwrap().eval = json!(f.eval);
    super::super::reconcile(
        Arc::new(f.eval.clone()),
        Arc::new(Ctx {
            client: f.client.clone(),
        }),
    )
    .await
    .unwrap();
    f.eval = serde_json::from_value(f.store.lock().unwrap().eval.clone()).unwrap();
    let claimed = f.eval.annotations()[workloads::RUN_TOKEN].clone();
    assert!(f.eval.status.is_none());
    assert_pending(&f.reconcile().await);
    assert!(!f.eval.annotations().contains_key(ANNOTATION_RUN_NOW));
    assert_eq!(f.eval.annotations()[workloads::LAST_TOKEN], claimed);
    let requested = f.eval.annotations()[workloads::LAST_RUN].clone();
    f.complete(&requested);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let record = stored(&f);
    assert_eq!(record.request_marker.as_deref(), Some(claimed.as_str()));
    assert_eq!(record.job_name, requested);
}

#[tokio::test]
async fn protected_same_job_receipt_survives_pod_and_job_gc_without_rewriting() {
    let mut f = fixture::setup().await;
    let requested = f.request().await;
    f.complete(&requested);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let original = stored(&f);
    f.store.lock().unwrap().pods.clear();
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    f.store.lock().unwrap().jobs.clear();
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    assert_eq!(stored(&f), original);
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
}

#[tokio::test]
async fn retained_same_name_receipt_cannot_authorize_a_replacement_job_uid() {
    let mut f = fixture::setup().await;
    let requested = f.request().await;
    f.complete(&requested);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let original = stored(&f);
    {
        let mut store = f.store.lock().unwrap();
        store.pods.clear();
        store.jobs.get_mut(&requested).unwrap()["metadata"]["uid"] = json!("replacement");
    }
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Degraded"));
    assert_eq!(stored(&f), original);
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
}

#[tokio::test]
async fn a_live_retained_job_with_changed_stamps_is_not_treated_as_ttl_gc() {
    let mut f = fixture::setup().await;
    let requested = f.request().await;
    f.complete(&requested);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let original = stored(&f);
    {
        let mut store = f.store.lock().unwrap();
        store.pods.clear();
        store.jobs.get_mut(&requested).unwrap()["spec"]["template"]["metadata"]["annotations"]
            [workloads::LAST_TOKEN] = json!("changed");
    }
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Degraded"));
    assert_eq!(stored(&f), original);
}

#[tokio::test]
async fn protected_pre_stamp_receipt_survives_gc_only_for_its_own_requested_job() {
    for same_job in [true, false] {
        let mut f = fixture::setup().await;
        let requested = f.request().await;
        f.complete(&requested);
        assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
        let mut legacy = stored(&f);
        legacy.request_job = None;
        legacy = legacy.seal().unwrap();
        f.eval.status.as_mut().unwrap().report_evidence_digest = Some(legacy.digest.clone());
        {
            let mut store = f.store.lock().unwrap();
            store.cms.get_mut(&evidence::name(&f.eval)).unwrap()["data"]["evidence.json"] =
                json!(serde_json::to_string(&legacy).unwrap());
            store.jobs.clear();
            store.pods.clear();
        }
        if !same_job {
            f.eval
                .annotations_mut()
                .insert(workloads::LAST_RUN.into(), "different-job".into());
        }
        let status = f.reconcile().await;
        if same_job {
            assert_eq!(status.phase.as_deref(), Some("Ready"));
        } else {
            assert_pending(&status);
        }
        assert_eq!(stored(&f), legacy);
        assert_eq!(f.store.lock().unwrap().report_writes, 1);
    }
}

#[tokio::test]
async fn retained_receipt_also_binds_the_requested_job_not_only_its_token() {
    let mut f = fixture::setup().await;
    let requested = f.request().await;
    f.complete(&requested);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let original = stored(&f);
    f.eval
        .annotations_mut()
        .insert(workloads::LAST_RUN.into(), "different-request".into());
    f.refresh_intent().await;
    f.store.lock().unwrap().jobs.clear();
    assert_pending(&f.reconcile().await);
    assert_eq!(stored(&f), original);
}

fn scheduled_job(f: &fixture::Fixture) -> String {
    let name = "scheduled-latest".to_string();
    {
        let mut store = f.store.lock().unwrap();
        let cron = store.crons[&cron_job_name("eval")].clone();
        let mut job = cron["spec"]["jobTemplate"].clone();
        job["apiVersion"] = json!("batch/v1");
        job["kind"] = json!("Job");
        job["metadata"]["name"] = json!(name);
        job["metadata"]["namespace"] = json!("tenant");
        job["metadata"]["uid"] = json!("scheduled-uid");
        job["metadata"]["resourceVersion"] = json!("60");
        job["metadata"]["generation"] = json!(1);
        job["metadata"]["creationTimestamp"] = json!("2026-09-10T20:00:01Z");
        job["metadata"]["ownerReferences"] = json!([{"apiVersion":"batch/v1","kind":"CronJob",
            "name":cron_job_name("eval"),"uid":cron["metadata"]["uid"],"controller":true}]);
        store.jobs.insert(name.clone(), job);
    }
    f.complete(&name);
    name
}

#[tokio::test]
async fn schedule_only_reconciliation_does_not_require_an_explicit_request() {
    for legacy_job_metadata in [false, true] {
        let mut f = fixture::setup().await;
        f.eval.spec.schedule = Some("*/15 * * * *".into());
        {
            let mut store = f.store.lock().unwrap();
            store.jobs.clear();
            store.pods.clear();
        }
        assert_pending(&f.reconcile().await);
        let scheduled = scheduled_job(&f);
        if legacy_job_metadata {
            f.store.lock().unwrap().jobs.get_mut(&scheduled).unwrap()["metadata"]["annotations"] =
                json!({});
        }
        assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
        let record = stored(&f);
        assert_eq!(record.job_name, scheduled);
        assert!(record.request_marker.is_none());
        assert!(record.request_job.is_none());
    }
}

#[tokio::test]
async fn scheduled_success_inherits_the_acknowledged_epoch_and_retains_its_own_job_uid() {
    let mut f = fixture::setup().await;
    f.eval.spec.schedule = Some("*/15 * * * *".into());
    let requested = f.request().await;
    f.complete(&requested);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let scheduled = scheduled_job(&f);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    let original = stored(&f);
    assert_eq!(original.job_name, scheduled);
    assert_eq!(original.job_uid, "scheduled-uid");
    assert_eq!(original.request_job.as_deref(), Some(requested.as_str()));
    assert_eq!(original.request_marker, f.intent.request_marker);
    {
        let mut store = f.store.lock().unwrap();
        store.jobs.clear();
        store.pods.clear();
    }
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Ready"));
    assert_eq!(stored(&f), original);
}

#[tokio::test]
async fn scheduled_success_cannot_hide_a_missing_or_running_explicit_request() {
    for missing in [false, true] {
        let mut f = fixture::setup().await;
        f.eval.spec.schedule = Some("*/15 * * * *".into());
        let requested = f.request().await;
        assert_pending(&f.reconcile().await);
        scheduled_job(&f);
        if missing {
            f.store.lock().unwrap().jobs.remove(&requested);
        }
        assert_pending(&f.reconcile().await);
        assert_eq!(f.store.lock().unwrap().report_writes, 0);
    }
}

#[tokio::test]
async fn request_stamp_changes_during_log_read_cannot_be_persisted() {
    let mut f = fixture::setup().await;
    let requested = f.request().await;
    {
        let mut store = f.store.lock().unwrap();
        store.jobs.retain(|name, _| name == &requested);
        store.pods.clear();
        store.mutate_on_log = Some("job-token");
    }
    f.complete(&requested);
    assert_eq!(f.reconcile().await.phase.as_deref(), Some("Degraded"));
    assert_eq!(f.store.lock().unwrap().report_writes, 0);
}
