// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

fn chain(count: usize) -> Vec<InclusionEntry> {
    let mut chain = Vec::new();
    for index in 0..count {
        chain.push(next_entry(&chain, &format!("ns/r{index}"), "digest"));
    }
    chain
}

fn map(index: usize, entries: &[InclusionEntry]) -> ConfigMap {
    let mut data = BTreeMap::from([(CHAIN_KEY.into(), serde_json::to_string(entries).unwrap())]);
    if index > 0 {
        data.insert("segmentIndex".into(), index.to_string());
        data.insert("previousRootHash".into(), entries[0].prev_hash.clone());
    }
    ConfigMap {
        metadata: kube::core::ObjectMeta {
            name: Some(segment_name(index)),
            namespace: Some(receipt_namespace()),
            uid: Some(format!("uid-{index}")),
            resource_version: Some("1".into()),
            labels: (index > 0)
                .then(|| BTreeMap::from([(COMPONENT_LABEL.into(), SEGMENT_COMPONENT.into())])),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    }
}

fn padded_map(bytes: usize) -> ConfigMap {
    let mut map = map(0, &chain(1));
    let raw = map.data.as_mut().unwrap().get_mut(CHAIN_KEY).unwrap();
    raw.push_str(&" ".repeat(bytes - raw.len()));
    map
}

#[test]
fn reads_legacy_and_out_of_order_segments_without_resetting_sequence() {
    let entries = chain(4);
    let state = parse_state(
        vec![
            map(2, &entries[3..]),
            map(0, &entries[..2]),
            map(1, &entries[2..3]),
        ],
        &receipt_namespace(),
    )
    .unwrap();
    assert_eq!(state.chain, entries);
    assert_eq!(state.segments.len(), 3);
    assert!(
        parse_state(Vec::new(), &receipt_namespace())
            .unwrap()
            .chain
            .is_empty()
    );
}

#[test]
fn rejects_corrupt_missing_duplicate_foreign_and_unbound_segments() {
    let entries = chain(3);
    let head = map(0, &entries[..1]);
    let overflow = map(1, &entries[1..]);
    let mut cases = vec![
        vec![overflow.clone()],
        vec![head.clone(), map(2, &entries[1..])],
        vec![head.clone(), head.clone()],
    ];
    for (key, value) in [
        (CHAIN_KEY, "invalid"),
        (CHAIN_KEY, "{}"),
        (CHAIN_KEY, "[]"),
        ("segmentIndex", "2"),
        ("previousRootHash", "genesis"),
    ] {
        let mut bad = overflow.clone();
        bad.data.as_mut().unwrap().insert(key.into(), value.into());
        cases.push(vec![head.clone(), bad]);
    }
    let mut missing = head.clone();
    missing.data = None;
    cases.push(vec![missing]);
    let mut bad_hash = head.clone();
    let mut forged = entries[..1].to_vec();
    forged[0].receipt = "different/receipt".into();
    bad_hash
        .data
        .as_mut()
        .unwrap()
        .insert(CHAIN_KEY.into(), serde_json::to_string(&forged).unwrap());
    cases.push(vec![bad_hash]);
    for field in ["name", "uid", "resourceVersion", "namespace"] {
        let mut bad = serde_json::to_value(&overflow).unwrap();
        bad["metadata"][field] = json!("foreign");
        // A changed UID/RV is valid identity; missing values are not.
        if matches!(field, "uid" | "resourceVersion") {
            bad["metadata"][field] = Value::Null;
        }
        cases.push(vec![head.clone(), serde_json::from_value(bad).unwrap()]);
    }
    let mut foreign = head.clone();
    foreign.metadata.labels = Some(BTreeMap::from([(COMPONENT_LABEL.into(), "other".into())]));
    cases.push(vec![foreign]);
    for maps in cases {
        assert!(parse_state(maps, &receipt_namespace()).is_err());
    }
}

#[derive(Default)]
struct Store {
    maps: BTreeMap<String, ConfigMap>,
    revision: u64,
    writes: Vec<(String, ConfigMap)>,
    fail_write: Option<u16>,
    compete_once: bool,
    list_failure: Option<u16>,
    truncated: bool,
}

fn status(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({
        "apiVersion": "v1", "kind": "Status", "status": "Failure",
        "reason": if code == 409 { "Conflict" } else { "Forbidden" }, "code": code,
        "message": "controlled API outcome"
    }))
}

fn respond(request: &Request, shared: &Arc<Mutex<Store>>) -> ResponseTemplate {
    let mut state = shared.lock().unwrap();
    let collection = format!("/api/v1/namespaces/{}/configmaps", receipt_namespace());
    if request.method == "GET" && request.url.path() == collection {
        if let Some(code) = state.list_failure {
            return status(code);
        }
        return ResponseTemplate::new(200).set_body_json(json!({
            "apiVersion": "v1", "kind": "ConfigMapList",
            "metadata": { "resourceVersion": state.revision.to_string(),
                "continue": if state.truncated { "next-page" } else { "" } },
            "items": state.maps.values().collect::<Vec<_>>(),
        }));
    }
    assert!(request.method == "PUT" || request.method == "POST");
    let mut incoming: ConfigMap = request.body_json().unwrap();
    state
        .writes
        .push((request.method.to_string(), incoming.clone()));
    if let Some(code) = state.fail_write {
        return status(code);
    }
    if state.compete_once {
        state.compete_once = false;
        let snapshot =
            parse_state(state.maps.values().cloned().collect(), &receipt_namespace()).unwrap();
        let mut entries = snapshot.chain.clone();
        entries.push(next_entry(&entries, "ns/competing", "other-digest"));
        let mut winner = map(0, &entries);
        state.revision += 1;
        winner.metadata.resource_version = Some(state.revision.to_string());
        state.maps.insert(LOG_CONFIGMAP_NAME.into(), winner);
        return status(409);
    }
    let name = incoming.name_any();
    if request.method == "PUT" {
        assert_eq!(request.url.path(), format!("{collection}/{name}"));
        let Some(existing) = state.maps.get(&name) else {
            return status(404);
        };
        if incoming.uid() != existing.uid()
            || incoming.resource_version() != existing.resource_version()
        {
            return status(409);
        }
        if existing.immutable == Some(true)
            && (incoming.data != existing.data || incoming.immutable != Some(true))
        {
            return status(422);
        }
    } else {
        assert_eq!(request.url.path(), collection);
        if state.maps.contains_key(&name) {
            return status(409);
        }
        incoming.metadata.uid = Some(format!("created-{name}"));
    }
    state.revision += 1;
    incoming.metadata.resource_version = Some(state.revision.to_string());
    state.maps.insert(name, incoming.clone());
    ResponseTemplate::new(if request.method == "POST" { 201 } else { 200 }).set_body_json(incoming)
}

async fn fixture(maps: Vec<ConfigMap>) -> (MockServer, Client, Arc<Mutex<Store>>) {
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(Store {
        maps: maps.into_iter().map(|map| (map.name_any(), map)).collect(),
        revision: 1,
        ..Default::default()
    }));
    let shared = state.clone();
    Mock::given(wiremock::matchers::any())
        .respond_with(move |request: &Request| respond(request, &shared))
        .mount(&server)
        .await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state)
}

#[tokio::test]
async fn creates_then_appends_with_uid_rv_and_preserves_legacy_metadata() {
    let (_server, client, state) = fixture(Vec::new()).await;
    assert!(read_chain(&client).await.unwrap().is_empty());
    let first = append(&client, "ns/first", "digest1").await.unwrap();
    assert_eq!(first.seq, 0);
    {
        let mut state = state.lock().unwrap();
        state
            .maps
            .get_mut(LOG_CONFIGMAP_NAME)
            .unwrap()
            .metadata
            .annotations = Some(BTreeMap::from([(
            "operator-note".into(),
            "retained".into(),
        )]));
    }
    let second = append(&client, "ns/second", "digest2").await.unwrap();
    assert_eq!(second.seq, 1);
    assert_eq!(
        read_chain(&client).await.unwrap(),
        vec![first.clone(), second]
    );
    assert_eq!(append(&client, "ns/first", "digest1").await.unwrap(), first);
    let state = state.lock().unwrap();
    assert_eq!(state.writes.len(), 2);
    assert_eq!(
        state.maps[LOG_CONFIGMAP_NAME].annotations()["operator-note"],
        "retained"
    );
    assert!(state.writes[1].1.uid().is_some());
    assert!(state.writes[1].1.resource_version().is_some());
}

#[tokio::test]
async fn exact_committed_threshold_seals_before_overflow_and_preserves_prefix() {
    for size in [
        MAX_SEGMENT_JSON_BYTES - 1,
        MAX_SEGMENT_JSON_BYTES,
        900 * 1024,
    ] {
        let original = padded_map(size);
        let original_data = original.data.clone();
        let (_server, client, shared) = fixture(vec![original]).await;
        let entry = append(&client, "ns/new", "new-digest").await.unwrap();
        assert_eq!(entry.seq, 1);
        assert_eq!(read_chain(&client).await.unwrap().last(), Some(&entry));
        let state = shared.lock().unwrap();
        if size < MAX_SEGMENT_JSON_BYTES {
            assert_eq!(state.maps.len(), 1);
            assert_ne!(state.maps[LOG_CONFIGMAP_NAME].immutable, Some(true));
            assert_eq!(state.writes.len(), 1);
        } else {
            assert_eq!(state.maps.len(), 2);
            assert_eq!(state.writes[0].0, "PUT");
            assert_eq!(state.writes[0].1.immutable, Some(true));
            assert_eq!(state.writes[0].1.data, original_data);
            assert_eq!(state.writes[1].0, "POST");
            assert_eq!(state.writes[1].1.name_any(), segment_name(1));
            assert_eq!(state.maps[LOG_CONFIGMAP_NAME].data, original_data);
            assert!(serde_json::to_vec(&state.writes[1].1).unwrap().len() < 1024 * 1024);
        }
    }
}

#[tokio::test]
async fn resumes_after_seal_and_keeps_idempotence_across_segments() {
    let mut head = padded_map(MAX_SEGMENT_JSON_BYTES);
    head.immutable = Some(true);
    let original = head.clone();
    let (_server, client, state) = fixture(vec![head]).await;
    let first = append(&client, "ns/new", "digest-new").await.unwrap();
    let repeated = append(&client, "ns/new", "digest-new").await.unwrap();
    assert_eq!(first, repeated);
    assert_eq!(append(&client, "ns/r0", "digest").await.unwrap().seq, 0);
    assert_eq!(
        append(&client, "ns/new", "digest-changed")
            .await
            .unwrap()
            .seq,
        2
    );
    let state = state.lock().unwrap();
    assert_eq!(state.maps[LOG_CONFIGMAP_NAME], original);
    assert_eq!(state.writes.len(), 2);
    assert!(
        state
            .writes
            .iter()
            .all(|(_, map)| map.name_any() == segment_name(1))
    );
}

#[tokio::test]
async fn real_conflict_reloads_winning_chain_before_next_sequence() {
    let (_server, client, state) = fixture(vec![map(0, &chain(1))]).await;
    state.lock().unwrap().compete_once = true;
    let entry = append(&client, "ns/ours", "ours-digest").await.unwrap();
    assert_eq!(entry.seq, 2);
    let entries = read_chain(&client).await.unwrap();
    assert_eq!(entries[1].receipt, "ns/competing");
    assert_eq!(entries[2], entry);
    assert_eq!(state.lock().unwrap().writes.len(), 2);
}

#[tokio::test]
async fn rejects_corruption_partial_reads_and_api_errors_without_writes() {
    for failure in [
        "malformed",
        "missing",
        "truncated",
        "forbidden",
        "oversized",
    ] {
        let mut head = map(0, &chain(1));
        if failure == "malformed" {
            head.data
                .as_mut()
                .unwrap()
                .insert(CHAIN_KEY.into(), "{secret-value".into());
        }
        let maps = if failure == "missing" {
            vec![map(1, &chain(2)[1..])]
        } else {
            vec![head]
        };
        let (_server, client, state) = fixture(maps).await;
        state.lock().unwrap().truncated = failure == "truncated";
        state.lock().unwrap().list_failure = (failure == "forbidden").then_some(403);
        let receipt = if failure == "oversized" {
            "x".repeat(MAX_ENTRY_JSON_BYTES)
        } else {
            "ns/new".into()
        };
        assert!(
            append(&client, &receipt, "digest").await.is_err(),
            "{failure}"
        );
        assert!(state.lock().unwrap().writes.is_empty(), "{failure}");
    }
}

#[tokio::test]
async fn write_errors_fail_and_only_conflicts_use_bounded_retries() {
    for code in [403, 409, 422, 500] {
        let (_server, client, state) = fixture(vec![map(0, &chain(1))]).await;
        state.lock().unwrap().fail_write = Some(code);
        assert!(append(&client, "ns/new", "digest").await.is_err());
        let state = state.lock().unwrap();
        assert_eq!(
            state.writes.len(),
            if code == 409 { MAX_APPEND_RETRIES } else { 1 }
        );
        assert_eq!(
            parse_state(state.maps.values().cloned().collect(), &receipt_namespace())
                .unwrap()
                .chain,
            chain(1)
        );
    }
}
