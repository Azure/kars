// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
mod fixture;

#[tokio::test]
async fn current_owned_producer_report_is_persisted_before_readiness_and_retries_are_idempotent() {
    let f = fixture::setup().await;
    let first = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    assert_eq!(first.state, "AllPassed");
    assert_eq!(first.result.as_ref().unwrap().passed, 6);
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
    let second = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    assert_eq!(second.state, "AllPassed");
    assert_eq!(second.history.len(), 1);
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
    let current = f.store.lock().unwrap().cms.values().next().unwrap().clone();
    let text = current["data"]["report.json"].as_str().unwrap();
    assert!(!text.contains("messages"));
    assert!(!text.contains("routerBase"));
    assert_eq!(current["metadata"]["ownerReferences"][0]["uid"], "eval-uid");
}

#[tokio::test]
async fn lost_evidence_ack_never_stamps_success_and_retry_reuses_the_written_record() {
    let f = fixture::setup().await;
    f.store.lock().unwrap().lose_ack = true;
    assert!(
        observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .is_err()
    );
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
    let result = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    assert_eq!(result.state, "AllPassed");
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
}

#[tokio::test]
async fn a_completed_runs_evidence_cannot_be_rewritten_on_a_later_reconcile() {
    let f = fixture::setup().await;
    observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    f.store.lock().unwrap().report["results"][0]["durationMs"] = json!(99);
    assert!(
        observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .is_err()
    );
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
}

#[tokio::test]
async fn failed_attempt_counter_is_not_terminal_but_failed_condition_is_inconclusive() {
    let f = fixture::setup().await;
    f.store.lock().unwrap().jobs.values_mut().next().unwrap()["status"] =
        json!({"failed":1,"active":1});
    let pending = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    assert_eq!(pending.state, "Pending");
    assert_eq!(f.store.lock().unwrap().report_writes, 0);
    {
        let mut state = f.store.lock().unwrap();
        state.jobs.values_mut().next().unwrap()["status"] = json!({"failed":1,"conditions":[
            {"type":"Failed","status":"True","lastTransitionTime":"2026-09-10T20:00:05Z"}]});
        state.pods.clear();
    }
    let result = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    assert_eq!(result.state, "Inconclusive");
    assert!(!result.drift);
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
}

#[tokio::test]
async fn legacy_v1_is_retained_but_never_ready_or_policy_drift() {
    let f = fixture::setup().await;
    f.store.lock().unwrap().report["schemaVersion"] = json!("v1");
    let result = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    assert_eq!(result.state, "RunnerUpgradeRequired");
    assert!(!result.drift);
    assert_eq!(result.result.unwrap().passed, 6);
    let text = f.store.lock().unwrap().cms.values().next().unwrap()["data"]["report.json"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.contains("\"schemaVersion\":\"v1\""));
}

#[tokio::test]
async fn wrong_current_eval_job_pod_or_target_identity_never_publishes_evidence() {
    for mutation in [
        "eval-uid",
        "eval-gen",
        "eval-spec",
        "target-uid",
        "target-gen",
        "target-spec",
        "job-uid",
        "job-gen",
        "pod-uid",
        "pod-gen",
        "pod-spec",
    ] {
        let f = fixture::setup().await;
        f.store.lock().unwrap().mutate_on_log = Some(mutation);
        assert!(
            observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
                .await
                .is_err(),
            "{mutation}"
        );
        assert_eq!(f.store.lock().unwrap().report_writes, 0, "{mutation}");
    }
}

#[tokio::test]
async fn foreign_latest_report_and_api_failures_are_not_adopted_or_ignored() {
    let f = fixture::setup().await;
    f.store.lock().unwrap().log_error = true;
    assert!(
        observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .is_err()
    );
    f.store.lock().unwrap().log_error = false;
    f.store.lock().unwrap().cms.insert(evidence::name(&f.eval), json!({
        "apiVersion":"v1","kind":"ConfigMap","metadata":{"name":evidence::name(&f.eval),"namespace":"tenant",
            "uid":"foreign","resourceVersion":"80","ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1",
                "kind":"KarsEval","name":"eval","uid":"old-eval","controller":true}]},
        "data":{"report.json":"private-foreign-content"},
    }));
    assert!(
        observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .is_err()
    );
    assert_eq!(f.store.lock().unwrap().report_writes, 0);
}

#[tokio::test]
async fn actual_cronjob_template_and_native_owner_chain_are_required_for_scheduled_results() {
    let mut f = fixture::setup().await;
    f.eval.spec.schedule = Some("*/15 * * * *".into());
    f.store.lock().unwrap().eval = json!(f.eval);
    f.intent = workloads::Intent::new(
        &f.client,
        &f.eval,
        &f.corpus.digest,
        "custom-numeric-user:latest",
        &sandbox_router_url("demo"),
    )
    .await
    .unwrap();
    let name = cron_job_name("eval");
    ensure_cronjob(
        &Api::<CronJob>::namespaced(f.client.clone(), "tenant"),
        &name,
        &f.eval,
        &f.intent,
        "*/15 * * * *",
        "karseval-eval-corpus",
        "custom-numeric-user:latest",
        &f.intent.router,
        &f.corpus.label,
    )
    .await
    .unwrap();
    {
        let mut store = f.store.lock().unwrap();
        let cron = store.crons[&name].clone();
        let job = store.jobs.values_mut().next().unwrap();
        job["spec"] = cron["spec"]["jobTemplate"]["spec"].clone();
        job["metadata"]["ownerReferences"] = json!([{"apiVersion":"batch/v1","kind":"CronJob",
            "name":name,"uid":cron["metadata"]["uid"],"controller":true}]);
        let template = job["spec"]["template"].clone();
        let pod = store.pods.values_mut().next().unwrap();
        pod["spec"] = template["spec"].clone();
        pod["metadata"]["annotations"] = template["metadata"]["annotations"].clone();
    }
    assert_eq!(
        observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .unwrap()
            .state,
        "AllPassed"
    );
    f.store.lock().unwrap().crons.get_mut(&name).unwrap()["metadata"]["uid"] = json!("replacement");
    assert!(
        observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .is_err()
    );
    assert_eq!(f.store.lock().unwrap().report_writes, 1);
}

#[tokio::test]
async fn bad_report_attribution_counts_and_exit_cannot_become_a_pass() {
    for (field, value) in [
        ("corpusDigest", json!("other")),
        ("total", json!(7)),
        ("routerBase", json!("other")),
        ("schemaVersion", json!("v3")),
    ] {
        let f = fixture::setup().await;
        f.store.lock().unwrap().report[field] = value;
        let result = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .unwrap();
        assert_eq!(result.state, "Inconclusive", "{field}");
        assert!(!result.drift);
    }
    let f = fixture::setup().await;
    f.store.lock().unwrap().pods.values_mut().next().unwrap()["status"]["containerStatuses"][0]["state"]
        ["terminated"]["exitCode"] = json!(2);
    assert_eq!(
        observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .unwrap()
            .state,
        "Inconclusive"
    );
}

#[tokio::test]
async fn validated_latest_evidence_survives_job_gc_but_not_a_new_intent() {
    let mut f = fixture::setup().await;
    let first = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    f.eval.status = Some(KarsEvalStatus {
        last_result: first.result,
        history: first.history,
        report_config_map_uid: first.receipt.as_ref().map(|receipt| receipt.uid.clone()),
        report_evidence_digest: first.receipt.as_ref().map(|receipt| receipt.digest.clone()),
        ..Default::default()
    });
    {
        let mut store = f.store.lock().unwrap();
        store.jobs.clear();
        store.pods.clear();
        store.eval = json!(f.eval);
    }
    assert_eq!(
        observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .unwrap()
            .state,
        "AllPassed"
    );
    f.eval.metadata.generation = Some(2);
    f.store.lock().unwrap().eval = json!(f.eval);
    let newer = workloads::Intent::new(
        &f.client,
        &f.eval,
        &f.corpus.digest,
        "custom-numeric-user:latest",
        &f.intent.router,
    )
    .await
    .unwrap();
    let stale = observation::observe(&f.client, &f.eval, &newer, &f.corpus)
        .await
        .unwrap();
    assert_ne!(stale.state, "AllPassed");
    assert_eq!(stale.history.len(), 1);
}

#[tokio::test]
async fn an_owned_cache_without_the_protected_status_receipt_cannot_create_fresh_ready() {
    let f = fixture::setup().await;
    observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    {
        let mut store = f.store.lock().unwrap();
        store.jobs.clear();
        store.pods.clear();
    }
    let result = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
        .await
        .unwrap();
    assert_eq!(result.state, "Inconclusive");
    assert!(result.receipt.is_none());
}

#[test]
fn inconclusive_and_legacy_results_never_emit_all_passed_conditions() {
    let resolved = ResolvedCorpus {
        bytes: vec![],
        digest: "sha256:current".into(),
        label: "builtin:current".into(),
    };
    let result = EvalResult {
        total: 1,
        passed: 0,
        failed: 0,
        errored: 1,
        ..Default::default()
    };
    for state in ["Inconclusive", "RunnerUpgradeRequired", "Pending"] {
        let conditions = build_conditions(
            &[],
            Some(1),
            &resolved,
            None,
            None,
            Some(&result),
            (state, false),
        );
        let ready = conditions
            .iter()
            .find(|condition| condition.type_ == "Ready")
            .unwrap();
        assert_eq!(ready.status, "False");
        assert!(
            conditions
                .iter()
                .all(|condition| condition.reason != "AllPassed")
        );
        assert!(
            conditions
                .iter()
                .filter(|condition| condition.type_ == TYPE_CONFORMANCE_DRIFT)
                .all(|c| c.status == "False")
        );
    }

    #[tokio::test]
    async fn error_only_and_mixed_terminal_reports_separate_inconclusive_from_real_drift() {
        for mixed in [false, true] {
            let f = fixture::setup().await;
            {
                let mut store = f.store.lock().unwrap();
                store.jobs.values_mut().next().unwrap()["status"] = json!({"failed":1,"conditions":[
                    {"type":"Failed","status":"True","lastTransitionTime":"2026-09-10T20:00:05Z"}]});
                let pod = store.pods.values_mut().next().unwrap();
                pod["status"]["phase"] = json!("Failed");
                pod["status"]["containerStatuses"][0]["state"]["terminated"]["exitCode"] = json!(2);
                for case in store.report["results"].as_array_mut().unwrap() {
                    case["actual"] = serde_json::Value::Null;
                    case["verdict"] = json!({"result":"Errored","category":"Upstream"});
                }
                store.report["passed"] = json!(0);
                store.report["errored"] = json!(6);
                if mixed {
                    store.report["results"][1]["actual"] = json!({"decision":"Allowed"});
                    store.report["results"][1]["verdict"] =
                        json!({"result":"Fail","reason":"DecisionMismatch"});
                    store.report["failed"] = json!(1);
                    store.report["errored"] = json!(5);
                }
            }
            let result = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
                .await
                .unwrap();
            assert_eq!(result.state, "Inconclusive");
            assert_eq!(result.drift, mixed);
            assert_eq!(result.result.unwrap().failed, if mixed { 1 } else { 0 });
        }
    }

    #[tokio::test]
    async fn request_claim_is_uid_rv_fenced_stable_across_retries_and_new_after_acknowledgement() {
        let mut f = fixture::setup().await;
        f.eval.metadata.annotations = Some([(ANNOTATION_RUN_NOW.into(), "true".into())].into());
        f.store.lock().unwrap().eval = json!(f.eval);
        let api = Api::<KarsEval>::namespaced(f.client.clone(), "tenant");
        assert!(workloads::claim_trigger(&api, &f.eval).await.unwrap());
        let claimed: KarsEval =
            serde_json::from_value(f.store.lock().unwrap().eval.clone()).unwrap();
        let token = claimed.annotations()[workloads::RUN_TOKEN].clone();
        assert!(!workloads::claim_trigger(&api, &claimed).await.unwrap());
        assert!(
            workloads::claim_trigger(&api, &f.eval).await.is_err(),
            "old RV must not overwrite the claim"
        );
        let acknowledged = workloads::acknowledge_trigger(&api, &claimed, "created-job")
            .await
            .unwrap();
        assert_eq!(
            acknowledged.annotations().get(workloads::LAST_TOKEN),
            Some(&token)
        );
        assert!(!acknowledged.annotations().contains_key(ANNOTATION_RUN_NOW));
        assert!(
            acknowledged.status.is_none(),
            "creation acknowledgement is not evaluation completion"
        );
        let mut next = acknowledged;
        next.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(ANNOTATION_RUN_NOW.into(), "true".into());
        f.store.lock().unwrap().eval = json!(next);
        workloads::claim_trigger(&api, &next).await.unwrap();
        let fresh: KarsEval = serde_json::from_value(f.store.lock().unwrap().eval.clone()).unwrap();
        assert_ne!(fresh.annotations().get(workloads::RUN_TOKEN), Some(&token));
    }

    #[tokio::test]
    async fn report_time_scope_and_retained_integrity_are_not_repaired_into_success() {
        let f = fixture::setup().await;
        f.store.lock().unwrap().report["startedAt"] = json!("2025-01-01T00:00:00Z");
        let result = observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
            .await
            .unwrap();
        assert_eq!(result.state, "Inconclusive");
        assert!(!result.drift);
        {
            let mut store = f.store.lock().unwrap();
            let cm = store.cms.values_mut().next().unwrap();
            let mut evidence: serde_json::Value =
                serde_json::from_str(cm["data"]["evidence.json"].as_str().unwrap()).unwrap();
            evidence["job_uid"] = json!("foreign");
            cm["data"]["evidence.json"] = json!(evidence.to_string());
        }
        assert!(
            observation::observe(&f.client, &f.eval, &f.intent, &f.corpus)
                .await
                .is_err()
        );
    }
}
