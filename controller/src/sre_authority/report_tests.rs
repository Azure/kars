// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    bindings, reports,
    tests::{fixture, registration},
};
use crate::sre_report::{Identity, KEY, NAME, NAMESPACE};
use k8s_openapi::api::core::v1::ServiceAccount;
use serde_json::{Value, json};

const REPORT: &str = "/api/v1/namespaces/kars-sre/configmaps/sre-health-report";
const ROLE: &str =
    "/apis/rbac.authorization.k8s.io/v1/namespaces/kars-sre/roles/sre-api-self-renew";

#[tokio::test]
async fn report_target_create_preserve_rotate_and_reject_foreign_owner() {
    let (_server, client, state) = fixture().await;
    let mut reg = registration();
    reports::ensure(&client, &reg).await.unwrap();
    let original = state.lock().unwrap().objects[REPORT].clone();
    assert_eq!(original["metadata"]["name"], NAME);
    assert_eq!(original["metadata"]["namespace"], NAMESPACE);
    state.lock().unwrap().objects.get_mut(REPORT).unwrap()["data"] =
        json!({KEY:"retained blocked evidence"});
    reports::ensure(&client, &reg).await.unwrap();
    assert_eq!(
        state.lock().unwrap().objects[REPORT]["data"][KEY],
        "retained blocked evidence"
    );
    assert_eq!(
        state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(method, _, _)| method == "POST")
            .count(),
        1
    );
    reg.spec.sandbox.uid = "replacement".into();
    {
        let mut state = state.lock().unwrap();
        state.sandbox["metadata"]["uid"] = json!("replacement");
        state.namespace["metadata"]["annotations"]["kars.azure.com/sandbox-uid"] =
            json!("replacement");
        state.objects.insert(
            "/apis/kars.azure.com/v1alpha1/karssreregistrations/canonical".into(),
            serde_json::to_value(&reg).unwrap(),
        );
    }
    reports::ensure(&client, &reg).await.unwrap();
    {
        let state = state.lock().unwrap();
        let rotated = &state.objects[REPORT];
        assert_eq!(rotated["metadata"]["uid"], original["metadata"]["uid"]);
        assert_eq!(rotated["metadata"]["resourceVersion"], "2");
        assert!(
            rotated["data"]
                .as_object()
                .is_none_or(|data| data.is_empty())
        );
        assert!(
            Identity::from_registration(&serde_json::to_value(&reg).unwrap())
                .unwrap()
                .matches_target(rotated)
        );
    }
    reg.metadata.uid = Some("new-registration".into());
    assert!(
        reports::ensure(&client, &reg)
            .await
            .unwrap_err()
            .contains("no adoption")
    );
}

fn role_rules() -> Value {
    json!([{"apiGroups":[""],"resources":["serviceaccounts/token"],"resourceNames":["sre-api-router"],"verbs":["create"]}])
}

pub(super) async fn grant_fixture() -> (
    wiremock::MockServer,
    kube::Client,
    std::sync::Arc<std::sync::Mutex<super::tests::State>>,
    ServiceAccount,
) {
    let (server, client, state) = fixture().await;
    for (name, rules) in [
        (
            "kars-sre-private-diagnostics",
            json!([{"apiGroups":[""],"resources":["pods"],"verbs":["get","list","watch"]}]),
        ),
        (
            "kars-sre-action-author",
            json!([{"apiGroups":["kars.azure.com"],"resources":["karssreactions"],"verbs":["create"]}]),
        ),
        (
            "kars-sre-router-renew",
            json!([{"apiGroups":["kars.azure.com"],"resources":["karssreregistrations"],"resourceNames":["canonical"],"verbs":["get","renew"]}]),
        ),
    ] {
        state.lock().unwrap().objects.insert(format!("/apis/rbac.authorization.k8s.io/v1/clusterroles/{name}"),json!({
            "apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRole","metadata":{"name":name},"rules":rules}));
    }
    let metadata = json!({"name":"sre-api-self-renew","namespace":"kars-sre","uid":"role-uid","resourceVersion":"1",
        "annotations":{"kars.azure.com/sre-registration-uid":"registration","kars.azure.com/namespace-uid":"runtime-ns"}});
    state.lock().unwrap().objects.insert(
        ROLE.into(),
        json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role",
        "metadata":metadata,"rules":role_rules()}),
    );
    let sa=serde_json::from_value(json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{
        "name":"sre-api-router","namespace":"kars-sre","uid":"router-sa","resourceVersion":"1",
        "annotations":{"kars.azure.com/sre-registration-uid":"registration","kars.azure.com/namespace-uid":"runtime-ns"}}})).unwrap();
    (server, client, state, sa)
}

#[tokio::test]
async fn report_grant_migrates_exact_owned_role_once_and_revokes_without_deleting_report() {
    let (_server, client, state, sa) = grant_fixture().await;
    let reg = registration();
    reports::ensure(&client, &reg).await.unwrap();
    bindings::validate_private_targets(&client, &reg)
        .await
        .unwrap();
    bindings::grant_private(&client, &reg, &sa).await.unwrap();
    bindings::grant_private(&client, &reg, &sa).await.unwrap();
    {
        let state = state.lock().unwrap();
        let role = &state.objects[ROLE];
        assert_eq!(role["metadata"]["uid"], "role-uid");
        assert_eq!(role["metadata"]["resourceVersion"], "2");
        assert_eq!(role["rules"][0], role_rules()[0]);
        assert_eq!(
            role["rules"][1],
            json!({"apiGroups":[""],"resources":["configmaps"],"resourceNames":[NAME],"verbs":["get","update"]})
        );
        assert_eq!(role["rules"].as_array().unwrap().len(), 2);
        assert_eq!(
            state
                .calls
                .iter()
                .filter(|(method, path, _)| method == "PUT" && path == ROLE)
                .count(),
            1
        );
    }
    bindings::revoke_private(&client, &reg).await.unwrap();
    let state = state.lock().unwrap();
    assert!(!state.objects.contains_key(ROLE));
    assert!(!state.objects.contains_key(
        "/apis/rbac.authorization.k8s.io/v1/namespaces/kars-sre/rolebindings/sre-api-self-renew"
    ));
    assert!(state.objects.contains_key(REPORT));
}

#[tokio::test]
async fn report_grant_rejects_foreign_or_excessive_roles_before_mutation() {
    for foreign in [true, false] {
        let (_server, client, state, _sa) = grant_fixture().await;
        {
            let mut state = state.lock().unwrap();
            let role = state.objects.get_mut(ROLE).unwrap();
            if foreign {
                role["metadata"]["annotations"]["kars.azure.com/sre-registration-uid"] =
                    json!("foreign");
            } else {
                role["rules"][0]["verbs"] = json!(["*"]);
            }
        }
        assert!(
            bindings::validate_private_targets(&client, &registration())
                .await
                .is_err()
        );
        assert!(
            state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(method, _, _)| method == "GET")
        );
    }
}
