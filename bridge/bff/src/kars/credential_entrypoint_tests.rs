use super::*;

const FIRST: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karsteams/first";
const GOOD: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/good";
const LATE: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/late";

fn source() -> Value {
    json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":"kars-credential-input-workspace","namespace":"work","uid":"source","resourceVersion":"1",
            "annotations":{"customer":"preserved"}},
        "data":{"SLACK_BOT_TOKEN":k8s_openapi::ByteString(b"original-value".to_vec())}})
}

fn prepare(state: &mut TestApi, existing: bool, conflict: bool) {
    state.objects.insert(GRANT.into(), grant(false));
    state.objects.insert(FIRST.into(),json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTeam",
        "metadata":{"name":"first","namespace":"work","uid":"first","resourceVersion":"1"},"spec":{"blueprint":{}}}));
    if existing {
        let source = source();
        state.objects.insert(SOURCE.into(), source.clone());
        acknowledge(state, &source);
        let mut good = sandbox(
            "good",
            json!({"credentialBindings":bindings("grant")}),
            true,
        );
        good["spec"]["credentialBindings"]["sources"][0]["keys"] = json!(["SLACK_BOT_TOKEN"]);
        state.objects.insert(GOOD.into(), good);
    } else {
        state
            .objects
            .insert(GOOD.into(), sandbox("good", json!({}), false));
    }
    if conflict {
        state.objects.insert(
            LATE.into(),
            sandbox(
                "late",
                json!({"credentialBindings":bindings("foreign")}),
                true,
            ),
        );
    }
}

#[tokio::test]
async fn credential_public_entrypoint_preflights_all_consumers_before_new_or_existing_source_mutation()
 {
    for existing in [false, true] {
        let (cluster, state, server) = fixture().await;
        let before = {
            let mut s = state.lock().unwrap();
            prepare(&mut s, existing, true);
            s.objects.clone()
        };
        let result = cluster
            .write_agent_credentials(
                "work",
                "Workspace",
                "work",
                None,
                BTreeMap::from([("SLACK_BOT_TOKEN".into(), "replacement-value".into())]),
                Vec::new(),
            )
            .await;
        assert!(result.is_err(), "existing source: {existing}");
        {
            let s = state.lock().unwrap();
            assert_eq!(
                s.objects, before,
                "source values, UIDs and all consumers must remain unchanged"
            );
            assert!(
                s.calls.iter().all(|(method, _, _)| method == "GET"),
                "no mutating request may precede full preflight"
            );
            assert!(
                s.calls
                    .iter()
                    .any(|(_, path, _)| path.ends_with("/karssandboxes"))
            );
            if existing {
                assert_eq!(s.objects[SOURCE]["metadata"]["uid"], "source");
                assert_eq!(
                    s.objects[SOURCE]["data"]["SLACK_BOT_TOKEN"],
                    json!(k8s_openapi::ByteString(b"original-value".to_vec()))
                );
            } else {
                assert!(!s.objects.contains_key(SOURCE));
            }
        }
        server.abort();
    }
}

#[tokio::test]
async fn credential_public_entrypoint_binds_only_real_source_uid_after_full_preflight_and_acknowledgement()
 {
    for existing in [false, true] {
        let (cluster, state, server) = fixture().await;
        {
            let mut s = state.lock().unwrap();
            prepare(&mut s, existing, false);
            s.objects.insert("/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/v1".into(),
                sandbox("v1",json!({"credentialsRef":{"name":"kars-credential-source-v1","uid":"v1-source"}}),true));
        }
        if !existing {
            // Native permissions deliberately do not reveal whether an
            // uninventoried source exists. CREATE must bootstrap without GET.
            let denied = kube::Api::<k8s_openapi::api::core::v1::Secret>::namespaced(
                cluster.client.clone(),
                "work",
            )
            .get_metadata("kars-credential-input-workspace")
            .await
            .unwrap_err();
            assert!(matches!(denied,kube::Error::Api(error) if error.code==403));
            state.lock().unwrap().calls.clear();
        }
        let result = cluster
            .write_agent_credentials(
                "work",
                "Workspace",
                "work",
                None,
                BTreeMap::from([("SLACK_BOT_TOKEN".into(), "requested-value".into())]),
                Vec::new(),
            )
            .await
            .unwrap();
        assert_eq!(result["stored"], true);
        {
            let s = state.lock().unwrap();
            let uid = if existing { "source" } else { "created-source" };
            assert_eq!(s.objects[SOURCE]["metadata"]["uid"], uid);
            assert_eq!(
                s.objects[FIRST]["spec"]["blueprint"]["credentialBindings"]["sources"][0]["source"]
                    ["uid"],
                uid
            );
            assert_eq!(
                s.objects[GOOD]["spec"]["credentialBindings"]["sources"][0]["source"]["uid"],
                uid
            );
            let first_write = s
                .calls
                .iter()
                .position(|(method, _, _)| method != "GET")
                .unwrap();
            assert_eq!(
                s.calls[first_write].1,
                if existing {
                    SOURCE
                } else {
                    "/api/v1/namespaces/work/secrets"
                }
            );
            for resource in ["karsteams", "karstasks", "karssandboxes"] {
                assert!(
                    s.calls[..first_write]
                        .iter()
                        .any(|(_, path, _)| path.ends_with(&format!("/{resource}")))
                );
            }
            let first_bind = s
                .calls
                .iter()
                .position(|(method, path, _)| method == "PATCH" && path == FIRST)
                .unwrap();
            assert!(
                s.calls[first_write + 1..first_bind]
                    .iter()
                    .any(|(method, path, _)| method == "GET" && path == SOURCE)
            );
            if !existing {
                assert!(
                    !s.calls[..first_write]
                        .iter()
                        .any(|(method, path, _)| method == "GET" && path == SOURCE)
                );
                let first_source_get = s
                    .calls
                    .iter()
                    .position(|(method, path, _)| method == "GET" && path == SOURCE)
                    .unwrap();
                assert!(
                    s.calls[first_write + 1..first_source_get]
                        .iter()
                        .any(|(method, path, _)| method == "GET" && path == GRANT)
                );
            }
            let legacy =
                &s.objects["/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/v1"];
            assert_eq!(legacy["spec"]["credentialsRef"]["uid"], "v1-source");
            assert!(legacy["spec"].get("credentialBindings").is_none());
        }
        server.abort();
    }
}

#[tokio::test]
async fn credential_public_entrypoint_rejects_missing_referenced_source_and_unobserved_existing_source_without_adoption()
 {
    for unobserved in [false, true] {
        let (cluster, state, server) = fixture().await;
        let before = {
            let mut s = state.lock().unwrap();
            prepare(&mut s, false, false);
            if unobserved {
                s.objects.insert(SOURCE.into(), source());
            } else {
                s.objects.insert(
                    LATE.into(),
                    sandbox(
                        "late",
                        json!({"credentialBindings":bindings("grant")}),
                        true,
                    ),
                );
            }
            s.objects.clone()
        };
        if unobserved {
            let denied = kube::Api::<k8s_openapi::api::core::v1::Secret>::namespaced(
                cluster.client.clone(),
                "work",
            )
            .get_metadata("kars-credential-input-workspace")
            .await
            .unwrap_err();
            assert!(matches!(denied,kube::Error::Api(error) if error.code==403));
            state.lock().unwrap().calls.clear();
        }
        let result = cluster
            .write_agent_credentials(
                "work",
                "Workspace",
                "work",
                None,
                BTreeMap::from([("SLACK_BOT_TOKEN".into(), "new-value".into())]),
                Vec::new(),
            )
            .await;
        assert!(result.is_err());
        if unobserved {
            assert!(matches!(result,Err(kube::Error::Api(error)) if error.code==409));
        }
        assert_eq!(state.lock().unwrap().objects, before);
        {
            let s = state.lock().unwrap();
            assert!(
                !s.calls
                    .iter()
                    .any(|(method, path, _)| method == "GET" && path == SOURCE)
            );
            if unobserved {
                assert_eq!(
                    s.calls
                        .iter()
                        .filter(|(method, _, _)| method == "POST")
                        .count(),
                    1
                );
                assert!(s.calls.iter().all(|(method, path, _)| method == "GET"
                    || (method == "POST" && path == "/api/v1/namespaces/work/secrets")));
            } else {
                assert!(s.calls.iter().all(|(method, _, _)| method == "GET"));
            }
        }
        server.abort();
    }
}
