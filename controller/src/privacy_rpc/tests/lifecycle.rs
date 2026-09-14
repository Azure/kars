// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

fn prepare_environment(data: &mut Data) {
    data.writes = true;
    data.objects.insert("/api/v1/namespaces/kars-system/pods/controller".into(),json!({
        "apiVersion":"v1","kind":"Pod","metadata":{"name":"controller","namespace":"kars-system","uid":"controller-pod",
            "resourceVersion":"1","labels":{"app.kubernetes.io/name":"kars","app.kubernetes.io/component":"controller"},
            "annotations":{crate::private_activation::EPOCH:"a".repeat(64)},
            "ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"qualified-controller",
                "uid":"qualified-controller-rs","controller":true}]},
        "spec":{"serviceAccountName":"kars-controller","containers":[{"name":"controller","image":"test:latest"}]}
    }));
    data.objects.insert("/apis/apps/v1/namespaces/kars-system/replicasets/qualified-controller".into(), json!({
        "apiVersion":"apps/v1","kind":"ReplicaSet",
        "metadata":{"name":"qualified-controller","namespace":"kars-system",
            "uid":"qualified-controller-rs","resourceVersion":"1"},
        "spec":{"template":{"metadata":{"annotations":{crate::private_activation::EPOCH:"a".repeat(64)}}}}
    }));
    data.objects.insert(
        "/apis/networking.k8s.io/v1/namespaces/kars-system/networkpolicies".into(),
        json!({
            "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicyList","metadata":{},"items":[{
                "metadata":{"name":"baseline","namespace":"kars-system","uid":"policy"},
                "spec":{"podSelector":{},"policyTypes":["Ingress","Egress"]}
            }]
        }),
    );
}

#[tokio::test]
async fn privacy_rpc_tls_rotation_recreation_and_runtime_publication_are_revision_bound() {
    let (_kube, state, data, request) = fixture().await;
    prepare_environment(&mut data.lock().unwrap());
    let first = identity::prepare(&state.client, "kars-system")
        .await
        .unwrap();
    publication::publish(
        &state.client,
        &first.endpoint,
        "controller",
        "controller-pod",
    )
    .await
    .unwrap();
    discovery::validate(&state.client, &first.endpoint)
        .await
        .unwrap();
    let same = identity::prepare(&state.client, "kars-system")
        .await
        .unwrap();
    assert_eq!(same.endpoint, first.endpoint);
    let path = format!("/api/v1/namespaces/kars-system/secrets/{}", wire::SECRET);
    data.lock().unwrap().objects.get_mut(&path).unwrap()["metadata"]["uid"] =
        "recreated-tls".into();
    let replacement = identity::prepare(&state.client, "kars-system")
        .await
        .unwrap();
    assert_ne!(replacement.endpoint.revision(), first.endpoint.revision());
    assert!(
        discovery::validate(&state.client, &first.endpoint)
            .await
            .is_err()
    );
    publication::publish(
        &state.client,
        &replacement.endpoint,
        "controller",
        "controller-pod",
    )
    .await
    .unwrap();
    discovery::validate(&state.client, &replacement.endpoint)
        .await
        .unwrap();
    publication::withdraw(&state.client, "kars-system", "controller", "controller-pod")
        .await
        .unwrap();
    assert!(
        data.lock().unwrap().objects["/api/v1/namespaces/kars-system/pods/controller"]["metadata"]
            ["labels"]
            .get(wire::REVISION_LABEL)
            .is_none()
    );
    assert_eq!(request.target.workspace, "workspace");
}

#[tokio::test]
async fn privacy_rpc_publication_never_adopts_another_pod_or_creates_namespace_isolation() {
    let (_kube, state, data, _request) = fixture().await;
    prepare_environment(&mut data.lock().unwrap());
    let prepared = identity::prepare(&state.client, "kars-system")
        .await
        .unwrap();
    assert!(
        publication::publish(&state.client, &prepared.endpoint, "controller", "wrong-pod")
            .await
            .is_err()
    );
    data.lock()
        .unwrap()
        .objects
        .get_mut("/apis/networking.k8s.io/v1/namespaces/kars-system/networkpolicies")
        .unwrap()["items"] = json!([]);
    assert!(
        publication::publish(
            &state.client,
            &prepared.endpoint,
            "controller",
            "controller-pod"
        )
        .await
        .is_err()
    );
    assert!(
        data.lock()
            .unwrap()
            .calls
            .iter()
            .all(|(method, path, _)| method == "GET" || !path.contains("/networkpolicies"))
    );
}

#[tokio::test]
async fn privacy_rpc_identity_refuses_unqualified_privacy_and_foreign_material_without_overwrite() {
    for fault in ["alias", "admission", "foreign"] {
        let (_kube, state, data, _request) = fixture().await;
        {
            let mut d = data.lock().unwrap();
            prepare_environment(&mut d);
            enroll(&mut d);
            match fault {
                "alias" => d.alias = true,
                "admission" => d.policy = true,
                _ => {
                    d.objects
                        .get_mut(&format!(
                            "/api/v1/namespaces/kars-system/secrets/{}",
                            wire::SECRET
                        ))
                        .unwrap()["metadata"]["annotations"][wire::CONTROLLER_UID] =
                        "foreign".into()
                }
            }
            d.calls.clear();
        }
        assert!(
            identity::prepare(&state.client, "kars-system")
                .await
                .is_err(),
            "{fault}"
        );
        assert!(
            data.lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, path, _)| method == "GET"
                    || path.ends_with("/subjectaccessreviews")
                    || path.ends_with("/selfsubjectreviews"))
        );
    }
}
