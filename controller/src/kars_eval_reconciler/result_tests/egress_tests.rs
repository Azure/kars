// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use wiremock::{
    Mock, Request, ResponseTemplate,
    matchers::{method, path, path_regex},
};

#[tokio::test]
async fn finalization_preserves_foreign_finalizers_and_fences_the_eval_incarnation() {
    for code in [200, 403, 409] {
        let mut f = fixture::setup().await;
        f.eval.metadata.finalizers = Some(vec![FINALIZER.into(), "another-controller".into()]);
        f.eval.metadata.deletion_timestamp =
            Some(serde_json::from_value(json!("2026-09-10T20:01:00Z")).unwrap());
        let expected = json!(f.eval);
        Mock::given(method("PATCH"))
            .and(path(
                "/apis/kars.azure.com/v1alpha1/namespaces/tenant/karsevals/eval",
            ))
            .respond_with(move |request: &Request| {
                assert_eq!(
                    request
                        .headers
                        .get("content-type")
                        .unwrap()
                        .to_str()
                        .unwrap(),
                    "application/merge-patch+json"
                );
                let patch: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(patch["metadata"]["uid"], "eval-uid");
                assert_eq!(patch["metadata"]["resourceVersion"], "10");
                assert_eq!(
                    patch["metadata"]["finalizers"],
                    json!(["another-controller"])
                );
                if code == 200 {
                    ResponseTemplate::new(code).set_body_json(&expected)
                } else {
                    ResponseTemplate::new(code).set_body_json(json!({
                        "apiVersion": "v1", "kind": "Status", "status": "Failure",
                        "code": code, "reason": "Rejected", "message": "rejected"
                    }))
                }
            })
            .with_priority(1)
            .expect(1)
            .mount(&f._server)
            .await;
        let count = f._server.received_requests().await.unwrap().len();
        let result = finalize(
            &Api::namespaced(f.client.clone(), "tenant"),
            &f.eval,
            "eval",
        )
        .await;
        assert_eq!(result.is_ok(), code == 200);
        assert!(
            f._server.received_requests().await.unwrap()[count..]
                .iter()
                .all(|request| request.method != "DELETE")
        );
    }
}

#[tokio::test]
async fn run_acknowledgement_preserves_proxy_binding_in_the_scheduled_producer() {
    let mut f = fixture::setup().await;
    f.eval.spec.corpus.builtin = Some("egress-known-bad".into());
    f.eval.spec.schedule = Some("0 * * * *".into());
    f.eval
        .annotations_mut()
        .insert(ANNOTATION_RUN_NOW.into(), "true".into());
    {
        let mut store = f.store.lock().unwrap();
        store.eval = json!(f.eval);
        store.jobs.clear();
        store.pods.clear();
    }
    let metadata = |name: &str, uid: &str| json!({"name": name, "namespace": "kars-demo", "uid": uid, "resourceVersion": "1"});
    let owner = |kind: &str, name: &str, uid: &str| json!([{"apiVersion": "apps/v1", "kind": kind, "name": name, "uid": uid, "controller": true}]);
    let mut rs = metadata("demo-rs", "rs-uid");
    rs["ownerReferences"] = owner("Deployment", "demo", "deployment-uid");
    let mut pod = metadata("demo-pod", "pod-uid");
    pod["ownerReferences"] = owner("ReplicaSet", "demo-rs", "rs-uid");
    let responses = [
        (
            "/api/v1/namespaces/kars-demo",
            json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {
                "name": "kars-demo", "uid": "namespace-uid", "resourceVersion": "1",
                "annotations": {
                    "kars.azure.com/namespace-claim-version": "v1",
                    "kars.azure.com/sandbox-namespace": "tenant",
                    "kars.azure.com/sandbox-name": "demo",
                    "kars.azure.com/sandbox-uid": "target-uid"
                }
            }}),
        ),
        (
            "/apis/apps/v1/namespaces/kars-demo/deployments/demo",
            json!({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": metadata("demo", "deployment-uid")}),
        ),
        (
            "/apis/apps/v1/namespaces/kars-demo/replicasets/demo-rs",
            json!({"apiVersion": "apps/v1", "kind": "ReplicaSet", "metadata": rs}),
        ),
        (
            "/api/v1/namespaces/kars-demo/pods",
            json!({"apiVersion": "v1", "kind": "PodList", "metadata": {}, "items": [{
                "apiVersion": "v1", "kind": "Pod", "metadata": pod,
                "status": {"phase": "Running", "containerStatuses": [{
                    "name": "inference-router", "ready": true, "restartCount": 0,
                    "image": "router", "imageID": "digest"
                }]}
            }]}),
        ),
    ];
    for (url, body) in responses {
        Mock::given(method("GET"))
            .and(path(url))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .with_priority(1)
            .mount(&f._server)
            .await;
    }
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../deploy/helm/kars/files/eval-proxy-admission.json"
    ))
    .unwrap();
    for (entry, resource) in [
        ("policy", "validatingadmissionpolicies"),
        ("binding", "validatingadmissionpolicybindings"),
    ] {
        let mut body = contract[entry].clone();
        body["metadata"]["generation"] = json!(1);
        body["status"] = json!({"observedGeneration": 1, "typeChecking": {}});
        Mock::given(method("GET"))
            .and(path(format!(
                "/apis/admissionregistration.k8s.io/v1/{resource}/kars-eval-proxy-port"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .with_priority(1)
            .mount(&f._server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path_regex(
            "^/(api/v1/namespaces/tenant/serviceaccounts|apis/rbac.authorization.k8s.io/v1/namespaces/kars-demo/(roles|rolebindings))$",
        ))
        .respond_with(|request: &Request| {
            let mut body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            body["metadata"]["uid"] = json!("grant-uid");
            body["metadata"]["resourceVersion"] = json!("1");
            ResponseTemplate::new(201).set_body_json(body)
        })
        .with_priority(1)
        .mount(&f._server)
        .await;
    super::super::reconcile(
        Arc::new(f.eval.clone()),
        Arc::new(Ctx {
            client: f.client.clone(),
        }),
    )
    .await
    .unwrap();
    f.eval = serde_json::from_value(f.store.lock().unwrap().eval.clone()).unwrap();
    f.reconcile().await;
    let store = f.store.lock().unwrap();
    assert!(!f.eval.annotations().contains_key(ANNOTATION_RUN_NOW));
    let requested = &store.jobs[&f.eval.annotations()[workloads::LAST_RUN]];
    let immediate = &requested["spec"]["template"];
    let scheduled =
        &store.crons.values().next().unwrap()["spec"]["jobTemplate"]["spec"]["template"];
    assert_eq!(immediate["spec"], scheduled["spec"]);
    assert_eq!(
        immediate["metadata"]["annotations"],
        scheduled["metadata"]["annotations"]
    );
    assert_eq!(scheduled["spec"]["automountServiceAccountToken"], true);
    assert!(
        scheduled["spec"]["serviceAccountName"]
            .as_str()
            .unwrap()
            .starts_with("karseval-proxy-")
    );
    let args = scheduled["spec"]["containers"][0]["args"]
        .as_array()
        .unwrap();
    assert!(
        args.windows(2)
            .any(|pair| pair == [json!("--forward-proxy-pod"), json!("kars-demo/demo-pod")])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == [json!("--forward-proxy-pod-uid"), json!("pod-uid")])
    );
}
