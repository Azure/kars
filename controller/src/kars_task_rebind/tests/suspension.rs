// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[tokio::test]
async fn credential_rebind_preserves_explicit_sandbox_suspension() {
    let (_server, ctx, state, team) = fixture().await;
    {
        let mut s = state.lock().unwrap();
        s.pods.clear();
        s.objects.get_mut(SANDBOX).unwrap()["spec"]["suspended"] = true.into();
    }
    let api = Api::<KarsTask>::namespaced(ctx.client.clone(), "work");
    crate::kars_team_reconciler::credential_bindings::reconcile(&ctx.client, &api, &team)
        .await
        .unwrap();
    crate::kars_task_reconciler::reconcile(Arc::new(current(&state)), ctx.clone())
        .await
        .unwrap();
    crate::kars_team_reconciler::credential_bindings::reconcile(&ctx.client, &api, &team)
        .await
        .unwrap();
    crate::kars_task_reconciler::reconcile(Arc::new(current(&state)), ctx.clone())
        .await
        .unwrap();
    let (sandbox, namespace, mut deployment): (
        crate::crd::KarsSandbox,
        k8s_openapi::api::core::v1::Namespace,
        k8s_openapi::api::apps::v1::Deployment,
    ) = {
        let s = state.lock().unwrap();
        assert_eq!(s.objects[SANDBOX]["spec"]["suspended"], true);
        (
            serde_json::from_value(s.objects[SANDBOX].clone()).unwrap(),
            serde_json::from_value(s.objects[RUNTIME].clone()).unwrap(),
            serde_json::from_value(s.objects[DEPLOYMENT].clone()).unwrap(),
        )
    };
    deployment.spec.as_mut().unwrap().replicas = Some(1);
    let identity =
        crate::reconciler::governed_services::identity_read_only(&ctx.client, &sandbox, &namespace)
            .await
            .unwrap();
    apply_deployment(&ctx.client, &sandbox, deployment, &identity)
        .await
        .unwrap();
    assert_eq!(
        state.lock().unwrap().objects[DEPLOYMENT]["spec"]["replicas"],
        0
    );
}
