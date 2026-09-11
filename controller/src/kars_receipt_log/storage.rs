// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{
    Client, ResourceExt,
    api::{Api, ListParams, PostParams},
};

use super::{
    CHAIN_KEY, GENESIS_PREV, InclusionEntry, LOG_CONFIGMAP_NAME, already_current, next_entry,
    receipt_namespace, verify_chain,
};

const SEGMENT_PREFIX: &str = "kars-receipt-log-";
const SEGMENT_COMPONENT: &str = "receipt-inclusion-log-segment";
const COMPONENT_LABEL: &str = "app.kubernetes.io/component";
const MAX_SEGMENT_JSON_BYTES: usize = 700 * 1024;
const MAX_ENTRY_JSON_BYTES: usize = 64 * 1024;
const MAX_APPEND_RETRIES: usize = 5;

struct Segment {
    map: ConfigMap,
    entries: Vec<InclusionEntry>,
}

struct LogState {
    chain: Vec<InclusionEntry>,
    segments: Vec<Segment>,
}

fn segment_name(index: usize) -> String {
    if index == 0 {
        LOG_CONFIGMAP_NAME.to_owned()
    } else {
        format!("{SEGMENT_PREFIX}{index:06}")
    }
}

fn parse_state(maps: Vec<ConfigMap>, namespace: &str) -> Result<LogState> {
    let mut ordered = BTreeMap::new();
    for map in maps {
        let name = map.name_any();
        let component = map.labels().get(COMPONENT_LABEL).map(String::as_str);
        let index = if name == LOG_CONFIGMAP_NAME {
            ensure!(
                component.is_none() || component == Some("receipt-inclusion-log"),
                "receipt log head has a foreign component"
            );
            0
        } else if let Some(suffix) = name.strip_prefix(SEGMENT_PREFIX) {
            let index: usize = suffix.parse().context("invalid receipt segment index")?;
            ensure!(
                index > 0 && name == segment_name(index) && component == Some(SEGMENT_COMPONENT),
                "invalid receipt segment identity"
            );
            index
        } else {
            ensure!(
                component != Some(SEGMENT_COMPONENT),
                "receipt segment label has an unexpected name"
            );
            continue;
        };
        ensure!(
            map.namespace().as_deref() == Some(namespace)
                && map.uid().is_some_and(|uid| !uid.is_empty())
                && map.resource_version().is_some_and(|rv| !rv.is_empty())
                && map.metadata.deletion_timestamp.is_none()
                && map.owner_references().is_empty(),
            "receipt segment lacks current namespace/identity or has a foreign owner"
        );
        ensure!(
            ordered.insert(index, map).is_none(),
            "duplicate receipt segment index"
        );
    }

    let mut chain = Vec::new();
    let mut segments = Vec::new();
    for (expected, (index, map)) in ordered.into_iter().enumerate() {
        ensure!(expected == index, "receipt log has a missing segment");
        let data = map.data.as_ref().context("receipt segment has no data")?;
        let raw = data
            .get(CHAIN_KEY)
            .context("receipt segment has no chain")?;
        let entries: Vec<InclusionEntry> =
            serde_json::from_str(raw).context("invalid receipt segment JSON")?;
        if index > 0 {
            ensure!(!entries.is_empty(), "receipt overflow segment is empty");
            ensure!(
                data.get("segmentIndex") == Some(&index.to_string()),
                "receipt segment index metadata mismatch"
            );
            let previous = chain
                .last()
                .map(|entry: &InclusionEntry| entry.entry_hash.as_str())
                .unwrap_or(GENESIS_PREV);
            ensure!(
                data.get("previousRootHash").map(String::as_str) == Some(previous),
                "receipt segment previous root mismatch"
            );
        }
        chain.extend(entries.iter().cloned());
        segments.push(Segment { map, entries });
    }
    verify_chain(&chain)
        .map_err(|seq| anyhow::anyhow!("receipt inclusion log is broken at seq {seq}"))?;
    Ok(LogState { chain, segments })
}

async fn read_state(cms: &Api<ConfigMap>) -> Result<LogState> {
    // One API snapshot includes the legacy head even when old replacements
    // dropped its labels. Separate head/overflow reads can mix log generations.
    let snapshot = cms.list(&ListParams::default()).await?;
    ensure!(
        snapshot.metadata.resource_version.is_some()
            && snapshot
                .metadata
                .continue_
                .as_deref()
                .unwrap_or("")
                .is_empty(),
        "receipt log snapshot is incomplete"
    );
    parse_state(snapshot.items, &receipt_namespace())
}

/// Append without resetting corrupt history. Only actual API conflicts retry;
/// sealed segments prevent older writers from changing a rotated prefix.
pub async fn append(
    client: &Client,
    receipt: &str,
    payload_sha256: &str,
) -> Result<InclusionEntry> {
    let cms = Api::<ConfigMap>::namespaced(client.clone(), &receipt_namespace());
    for _ in 0..MAX_APPEND_RETRIES {
        let state = read_state(&cms).await?;
        if already_current(&state.chain, receipt, payload_sha256) {
            return state
                .chain
                .into_iter()
                .rev()
                .find(|entry| entry.receipt == receipt)
                .context("current receipt entry is missing");
        }
        let entry = next_entry(&state.chain, receipt, payload_sha256);
        ensure!(
            serde_json::to_vec(&entry)?.len() <= MAX_ENTRY_JSON_BYTES,
            "receipt inclusion entry exceeds its storage budget"
        );
        let active = state.segments.last();
        // Rotation depends only on committed state, not competing entry sizes.
        // Seal first with UID/RV; a crash here is resumed by the next append.
        let seal = state.segments.iter().enumerate().find(|(index, segment)| {
            segment.map.immutable != Some(true)
                && (*index + 1 < state.segments.len()
                    || segment
                        .map
                        .data
                        .as_ref()
                        .and_then(|d| d.get(CHAIN_KEY))
                        .is_some_and(|raw| raw.len() >= MAX_SEGMENT_JSON_BYTES))
        });
        if let Some((_, segment)) = seal {
            let mut map = segment.map.clone();
            map.immutable = Some(true);
            match cms
                .replace(&map.name_any(), &PostParams::default(), &map)
                .await
            {
                Ok(_) => continue,
                Err(kube::Error::Api(error)) if error.code == 409 => continue,
                Err(error) => return Err(error).context("sealing receipt inclusion segment"),
            }
        }

        let result = if let Some(segment) = active.filter(|s| s.map.immutable != Some(true)) {
            let mut entries = segment.entries.clone();
            entries.push(entry.clone());
            let mut map = segment.map.clone();
            map.data
                .get_or_insert_default()
                .insert(CHAIN_KEY.to_owned(), serde_json::to_string(&entries)?);
            cms.replace(&map.name_any(), &PostParams::default(), &map)
                .await
        } else {
            let index = state.segments.len();
            let mut data = BTreeMap::from([(
                CHAIN_KEY.to_owned(),
                serde_json::to_string(std::slice::from_ref(&entry))?,
            )]);
            if index > 0 {
                data.insert("segmentIndex".into(), index.to_string());
                data.insert("previousRootHash".into(), entry.prev_hash.clone());
            }
            let map = ConfigMap {
                metadata: kube::core::ObjectMeta {
                    name: Some(segment_name(index)),
                    namespace: Some(receipt_namespace()),
                    labels: Some(BTreeMap::from([
                        ("app.kubernetes.io/name".into(), "kars".into()),
                        (
                            COMPONENT_LABEL.into(),
                            if index == 0 {
                                "receipt-inclusion-log"
                            } else {
                                SEGMENT_COMPONENT
                            }
                            .into(),
                        ),
                    ])),
                    ..Default::default()
                },
                data: Some(data),
                ..Default::default()
            };
            cms.create(&PostParams::default(), &map).await
        };
        match result {
            Ok(_) => return Ok(entry),
            Err(kube::Error::Api(error)) if error.code == 409 => continue,
            Err(error) => return Err(error).context("appending receipt inclusion entry"),
        }
    }
    anyhow::bail!("receipt inclusion log append exhausted bounded retries")
}

/// Read and verify every segment from the same API snapshot.
pub async fn read_chain(client: &Client) -> Result<Vec<InclusionEntry>> {
    let cms = Api::<ConfigMap>::namespaced(client.clone(), &receipt_namespace());
    Ok(read_state(&cms).await?.chain)
}

#[cfg(test)]
mod tests;
