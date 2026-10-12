// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{
    body::Body,
    http::{Request, Response, StatusCode},
};
use serde_json::{Value, json};
use std::convert::Infallible;

fn fixture() -> (ObjectMeta, ConfigMap) {
    let content = "# Decision\n日本語\n";
    let team: ObjectMeta = serde_json::from_value(json!({
        "name": "clinic", "namespace": "tenant-a", "uid": "team-original"
    }))
    .unwrap();
    let cm = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": {
            "name": "kars-commons-custom", "namespace": "tenant-a",
            "uid": "store-original", "resourceVersion": "7",
            "ownerReferences": [{
                "apiVersion": "kars.azure.com/v1alpha1", "kind": "KarsTeam",
                "name": "clinic", "uid": "team-original", "controller": true
            }]
        },
        "data": {
            "index.json": json!([{
                "id": "run_one", "title": "Decision", "author": "clinic-run-1",
                "source_task": "clinic-run-1", "created_at": "2026-10-12T08:00:00Z",
                "digest": "sha256:ae18a664629289ab2fc86a803f0021da",
                "size_bytes": content.len()
            }]).to_string(),
            "entry-run_one": content
        }
    }))
    .unwrap();
    (team, cm)
}

#[test]
fn commons_accepts_custom_names_in_the_actual_team_namespace() {
    let (team, cm) = fixture();
    validate_commons(&cm, &team, "custom").unwrap();
    assert!(validate_commons(&cm, &team, "clinic").is_err());
}

#[test]
fn commons_accepts_the_retained_live_controller_content_address() {
    let (team, mut cm) = fixture();
    let content =
        "Completed and attached:\n- clinic-review.md\n- decision-log.md\n- opening-checklist.md";
    assert_eq!(content.len(), 83);
    let data = cm.data.as_mut().unwrap();
    let mut index: Value = serde_json::from_str(&data["index.json"]).unwrap();
    index[0]["digest"] = "sha256:96db76a4dc80f8817a98e921828ca2a6".into();
    index[0]["size_bytes"] = 83.into();
    data.insert("index.json".into(), index.to_string());
    data.insert("entry-run_one".into(), content.into());
    validate_commons(&cm, &team, "custom").unwrap();
}

#[test]
fn commons_validates_full_archive_bytes_and_source_without_requiring_live_tasks() {
    let (team, mut cm) = fixture();
    let content = "Full retained decision\r\n日本語\n";
    let document = json!({
        "name":"decision-log.md", "content":content, "sha256":crate::team_memory::digest(content),
        "source":{"namespace":"tenant-a","taskName":"clinic-run-1","taskUid":"collected-task",
            "sandboxUid":"sandbox","podUid":"pod","runNonce":"run-1","assignmentId":"assignment-1",
            "agentDid":format!("did:mesh:{}", "a".repeat(32)),"artifactName":"decision-log.md"}
    });
    let mut peer = document.clone();
    peer["name"] = "r0-p0.md".into();
    peer["source"]["taskName"] = "peer".into();
    peer["source"]["artifactName"] = "handoff.md".into();
    let archive = json!({"version":1,"output":"Completed","output_sha256":crate::team_memory::digest("Completed"),
        "documents":[document],"handoffs":[peer]});
    let key = crate::team_memory::archive_key("run_one");
    cm.data
        .as_mut()
        .unwrap()
        .insert(key.clone(), archive.to_string());
    validate_commons(&cm, &team, "custom").unwrap();
    for (path, value) in [
        ("/documents/0/content", json!("substituted")),
        ("/documents/0/source/taskName", json!("different-run")),
        ("/documents/0/source/namespace", json!("different-tenant")),
        ("/handoffs/0/sha256", json!("wrong")),
        ("/output_sha256", json!("wrong")),
    ] {
        let mut broken = archive.clone();
        *broken.pointer_mut(path).unwrap() = value;
        cm.data
            .as_mut()
            .unwrap()
            .insert(key.clone(), broken.to_string());
        assert!(validate_commons(&cm, &team, "custom").is_err(), "{path}");
    }
    cm.data.as_mut().unwrap().insert(key, archive.to_string());
    cm.data.as_mut().unwrap().insert(
        crate::team_memory::archive_key("unindexed"),
        archive.to_string(),
    );
    assert!(validate_commons(&cm, &team, "custom").is_err());
}

#[test]
fn commons_rejects_foreign_replaced_or_unidentified_stores() {
    let (team, cm) = fixture();
    for (path, value) in [
        ("/metadata/name", json!("kars-commons-other")),
        ("/metadata/namespace", json!("kars-system")),
        ("/metadata/uid", Value::Null),
        ("/metadata/resourceVersion", json!("")),
        ("/metadata/ownerReferences/0/name", json!("other")),
        ("/metadata/ownerReferences/0/uid", json!("team-recreated")),
        ("/metadata/ownerReferences/0/kind", json!("KarsTask")),
        (
            "/metadata/ownerReferences/0/apiVersion",
            json!("foreign/v1"),
        ),
        ("/metadata/ownerReferences/0/controller", json!(false)),
    ] {
        let mut broken = serde_json::to_value(&cm).unwrap();
        *broken.pointer_mut(path).unwrap() = value;
        assert!(
            validate_commons(&serde_json::from_value(broken).unwrap(), &team, "custom").is_err(),
            "{path}"
        );
    }
    let mut broken = cm.clone();
    broken
        .metadata
        .owner_references
        .as_mut()
        .unwrap()
        .push(cm.metadata.owner_references.as_ref().unwrap()[0].clone());
    assert!(validate_commons(&broken, &team, "custom").is_err());
    broken = cm;
    broken.metadata.deletion_timestamp =
        Some(serde_json::from_value(json!("2026-10-12T08:00:00Z")).unwrap());
    assert!(validate_commons(&broken, &team, "custom").is_err());
}

#[test]
fn commons_rejects_missing_malformed_or_unindexed_content() {
    let (team, cm) = fixture();
    for field in ["index.json", "entry-run_one"] {
        let mut broken = cm.clone();
        broken.data.as_mut().unwrap().remove(field);
        assert!(
            validate_commons(&broken, &team, "custom").is_err(),
            "{field}"
        );
    }
    for (key, value) in [
        ("index.json", "{"),
        ("entry-run_one", "changed"),
        ("entry-hidden", "unindexed"),
    ] {
        let mut broken = cm.clone();
        broken
            .data
            .as_mut()
            .unwrap()
            .insert(key.into(), value.into());
        assert!(validate_commons(&broken, &team, "custom").is_err(), "{key}");
    }
}

#[test]
fn commons_checks_all_entry_metadata_and_normalized_key_collisions() {
    let (team, cm) = fixture();
    for (field, value) in [
        ("digest", json!("sha256:wrong")),
        ("size_bytes", json!(1)),
        ("created_at", json!("not-a-time")),
        ("source_task", Value::Null),
        ("id", json!("")),
    ] {
        let mut broken = cm.clone();
        let data = broken.data.as_mut().unwrap();
        let mut index: Value = serde_json::from_str(&data["index.json"]).unwrap();
        index[0][field] = value;
        data.insert("index.json".into(), index.to_string());
        assert!(
            validate_commons(&broken, &team, "custom").is_err(),
            "{field}"
        );
    }
    for alternate_id in ["run_one", "run/one"] {
        let mut broken = cm.clone();
        let data = broken.data.as_mut().unwrap();
        let mut index: Vec<Value> = serde_json::from_str(&data["index.json"]).unwrap();
        let mut duplicate = index[0].clone();
        duplicate["id"] = alternate_id.into();
        index.push(duplicate);
        data.insert("index.json".into(), serde_json::to_string(&index).unwrap());
        assert!(validate_commons(&broken, &team, "custom").is_err());
    }
}

fn cluster(status: StatusCode, body: String) -> Cluster {
    let service = tower::service_fn(move |request: Request<kube::client::Body>| {
        let body = body.clone();
        async move {
            assert_eq!(request.method(), "GET");
            assert_eq!(
                request.uri().path(),
                "/api/v1/namespaces/tenant-a/configmaps/kars-commons-custom"
            );
            Ok::<_, Infallible>(
                Response::builder()
                    .status(status)
                    .body(Body::from(body))
                    .unwrap(),
            )
        }
    });
    Cluster::for_test_client(kube::Client::new(service, "unused-default"))
}

#[tokio::test]
async fn commons_read_preserves_absence_and_propagates_api_failures() {
    let (team, cm) = fixture();
    let actual = cluster(StatusCode::OK, serde_json::to_string(&cm).unwrap())
        .read_commons(&team, "custom")
        .await
        .unwrap();
    assert_eq!(actual, cm.data);
    for status in [
        StatusCode::NOT_FOUND,
        StatusCode::FORBIDDEN,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let body = json!({
            "apiVersion": "v1", "kind": "Status", "status": "Failure",
            "reason": if status == StatusCode::NOT_FOUND { "NotFound" } else { "test" },
            "message": "test", "code": status.as_u16()
        })
        .to_string();
        let result = cluster(status, body).read_commons(&team, "custom").await;
        if status == StatusCode::NOT_FOUND {
            assert!(result.unwrap().is_none());
        } else {
            assert!(result.is_err(), "{status}");
        }
    }
    let mut broken = cm;
    broken
        .data
        .as_mut()
        .unwrap()
        .insert("index.json".into(), "{".into());
    assert!(
        cluster(StatusCode::OK, serde_json::to_string(&broken).unwrap())
            .read_commons(&team, "custom")
            .await
            .is_err()
    );
}
