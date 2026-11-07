// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::body::Bytes;
use serde_json::{Value, json};

const PRIVATE: &str = "PRIVATE_CONFIGMAP_PAYLOAD";

fn map(name: &str) -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap",
        "metadata":{"name":name,"namespace":"work","uid":name,"resourceVersion":"7"},
        "data":{"value":"retained αβ"}})
}

fn snapshot() -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMapList",
        "metadata":{"resourceVersion":"snapshot-7"},"items":[map("keep")]})
}

#[tokio::test]
async fn snapshot_filters_during_transfer_without_buffering_unrelated_history() {
    let (first_processed, acknowledged) = tokio::sync::oneshot::channel();
    let stream = async_stream::try_stream! {
        yield Bytes::from_static(b"{\"apiVersion\":\"v1\",\"kind\":\"ConfigMapList\",\"metadata\":{\"resourceVersion\":\"7\"},\"items\":[");
        yield Bytes::from(serde_json::to_vec(&map("first")).unwrap());
        yield Bytes::from_static(b",");
        // A whole-body reader deadlocks here: the next bytes are not sent until
        // the first item has actually been decoded and filtered.
        acknowledged.await.map_err(std::io::Error::other)?;
        let mut unrelated = map("unrelated-budget-history");
        unrelated["data"]["value"] = "x".repeat(4096).into();
        let unrelated = Bytes::from(format!("{},", unrelated));
        for _ in 0..4096 {
            yield unrelated.clone();
        }
        yield Bytes::from(serde_json::to_vec(&map("last")).unwrap());
        yield Bytes::from_static(b"]}");
    };
    let stream: std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<Bytes, std::io::Error>> + Send>,
    > = Box::pin(stream);
    let body = Body::from_stream(stream);
    let mut first_processed = Some(first_processed);
    let read = decode_snapshot(body, "work".into(), move |map| {
        let name = map.metadata.name.as_deref().unwrap();
        if name == "first" {
            first_processed.take().unwrap().send(()).unwrap();
        }
        matches!(name, "first" | "last").then_some(map)
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), read)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.metadata.resource_version.as_deref(), Some("7"));
    assert_eq!(result.items.len(), 2);
    assert_eq!(result.items[1].metadata.name.as_deref(), Some("last"));
}

#[tokio::test]
async fn snapshot_preserves_selected_bytes_across_utf8_and_json_chunk_boundaries() {
    let value = snapshot();
    let raw = serde_json::to_vec(&value).unwrap();
    let stream = tokio_stream::iter(
        raw.into_iter()
            .map(|byte| Ok::<_, std::io::Error>(Bytes::from(vec![byte]))),
    );
    let result = decode_snapshot(Body::from_stream(stream), "work".into(), Some)
        .await
        .unwrap();
    assert_eq!(result.items.len(), 1);
    assert_eq!(
        result.items[0].data.as_ref().unwrap()["value"],
        "retained αβ"
    );
}

#[tokio::test]
async fn snapshot_rejects_incomplete_foreign_and_malformed_data_without_partial_success() {
    let mut cases = Vec::new();
    for key in ["apiVersion", "kind", "metadata", "items"] {
        let mut value = snapshot();
        value.as_object_mut().unwrap().remove(key);
        cases.push(value.to_string());
    }
    for value in [Value::Null, json!({}), json!(PRIVATE)] {
        let mut current = snapshot();
        current["items"] = value;
        cases.push(current.to_string());
    }
    for (key, replacement) in [("apiVersion", "foreign/v1"), ("kind", "SecretList")] {
        let mut value = snapshot();
        value[key] = replacement.into();
        cases.push(value.to_string());
    }
    let mut value = snapshot();
    value["metadata"]["continue"] = PRIVATE.into();
    cases.push(value.to_string());
    let mut value = snapshot();
    value["metadata"] = json!({});
    cases.push(value.to_string());
    let mut value = snapshot();
    value["items"][0]["metadata"]["namespace"] = "foreign".into();
    cases.push(value.to_string());
    let mut value = snapshot();
    value["items"][0]["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("name");
    cases.push(value.to_string());
    let mut value = snapshot();
    value["items"][0]["data"] = PRIVATE.into();
    cases.push(value.to_string());
    let raw = snapshot().to_string();
    cases.push(format!("{raw} {PRIVATE}"));
    cases.push(raw[..raw.len() - 1].to_string());
    for duplicate in [
        "\"items\":[]",
        "\"kind\":\"ConfigMapList\"",
        "\"apiVersion\":\"v1\"",
        "\"metadata\":{\"resourceVersion\":\"7\"}",
    ] {
        cases.push(format!("{},{}{}", &raw[..raw.len() - 1], duplicate, "}"));
    }
    for raw in cases {
        let error = decode_snapshot(Body::from(raw), "work".into(), Some)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "ConfigMap snapshot transport or decoding failed"
        );
        assert!(!error.to_string().contains(PRIVATE));
    }
}

#[tokio::test]
async fn snapshot_transport_failure_never_returns_selected_prefix_or_private_error() {
    let stream = tokio_stream::iter([
        Ok(Bytes::from_static(b"{\"items\":[")),
        Ok(Bytes::from(serde_json::to_vec(&map("keep")).unwrap())),
        Err(std::io::Error::other(PRIVATE)),
    ]);
    let error = decode_snapshot(Body::from_stream(stream), "work".into(), Some)
        .await
        .unwrap_err();
    assert!(!error.to_string().contains(PRIVATE));
}

#[tokio::test]
async fn snapshot_validates_unselected_objects_and_accepts_an_empty_complete_snapshot() {
    let mut value = snapshot();
    value["items"][0]["metadata"]["namespace"] = "foreign".into();
    assert!(
        decode_snapshot(Body::from(value.to_string()), "work".into(), |_| None::<
            String,
        >)
        .await
        .is_err()
    );
    value["items"] = json!([]);
    let result = decode_snapshot(Body::from(value.to_string()), "work".into(), Some)
        .await
        .unwrap();
    assert!(result.items.is_empty());
}
