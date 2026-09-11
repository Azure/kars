// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! One complete, namespace-bound snapshot for receipt-log readers and summaries.

use std::collections::BTreeMap;

use crate::providers::signing::sha256_parts;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ListMeta;
use kube::{Api, api::ListParams};
use serde::Deserialize;

use super::cluster::Cluster;

const HEAD: &str = "kars-receipt-log";
const PREFIX: &str = "kars-receipt-log-";
const COMPONENT: &str = "app.kubernetes.io/component";
const SEGMENT_COMPONENT: &str = "receipt-inclusion-log-segment";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChainEntry {
    pub seq: i64,
    pub receipt: String,
    pub payload_sha256: String,
    pub prev_hash: String,
    pub entry_hash: String,
}

pub(crate) fn chain_entry_hash(
    seq: i64,
    receipt: &str,
    payload_sha256: &str,
    prev_hash: &str,
) -> String {
    let sequence = seq.to_string();
    hex::encode(sha256_parts([
        sequence.as_bytes(),
        b"|",
        receipt.as_bytes(),
        b"|",
        payload_sha256.as_bytes(),
        b"|",
        prev_hash.as_bytes(),
    ]))
}

#[derive(Debug, Default)]
pub(crate) struct ReceiptLog {
    pub present: bool,
    pub entries: Vec<ChainEntry>,
    pub checkpoint: Option<BTreeMap<String, String>>,
    pub witness: Option<BTreeMap<String, String>>,
    pub public_key: Option<BTreeMap<String, String>>,
}

impl ReceiptLog {
    pub fn anchor(&self) -> Option<(String, String, String)> {
        let data = self.public_key.as_ref()?;
        Some((
            data.get("keyId")?.clone(),
            data.get("publicKey")?.clone(),
            data.get("scheme").cloned().unwrap_or_default(),
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReceiptLogError {
    #[error("receipt log snapshot API status {0}")]
    Api(u16),
    #[error("receipt log snapshot transport or decoding failed")]
    Read,
    #[error("invalid receipt log: {0}")]
    Invalid(&'static str),
}

fn invalid(message: &'static str) -> ReceiptLogError {
    ReceiptLogError::Invalid(message)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceiptLogSnapshot {
    api_version: String,
    kind: String,
    metadata: ListMeta,
    items: Vec<ConfigMap>,
}

fn validate_identity(map: &ConfigMap, namespace: &str) -> Result<(), ReceiptLogError> {
    if map.metadata.namespace.as_deref() != Some(namespace)
        || map.metadata.name.as_deref().is_none_or(str::is_empty)
        || map.metadata.uid.as_deref().is_none_or(str::is_empty)
        || map
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || map.metadata.deletion_timestamp.is_some()
    {
        return Err(invalid("missing or foreign ConfigMap identity"));
    }
    Ok(())
}

fn parse_snapshot(
    snapshot: ReceiptLogSnapshot,
    namespace: &str,
) -> Result<ReceiptLog, ReceiptLogError> {
    if snapshot.api_version != "v1"
        || snapshot.kind != "ConfigMapList"
        || snapshot
            .metadata
            .resource_version
            .as_deref()
            .is_none_or(str::is_empty)
        || snapshot
            .metadata
            .continue_
            .as_deref()
            .is_some_and(|value| !value.is_empty())
    {
        return Err(invalid("incomplete or foreign API snapshot"));
    }
    let mut log = ReceiptLog::default();
    let mut segments = BTreeMap::new();
    for map in snapshot.items {
        if map.metadata.namespace.as_deref() != Some(namespace) {
            return Err(invalid("foreign namespace in API snapshot"));
        }
        let name = map
            .metadata
            .name
            .as_deref()
            .ok_or_else(|| invalid("unnamed ConfigMap"))?;
        let component = map
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get(COMPONENT))
            .map(String::as_str);
        let index = if name == HEAD {
            if component.is_some_and(|value| value != "receipt-inclusion-log") {
                return Err(invalid("foreign head component"));
            }
            Some(0)
        } else if let Some(suffix) = name.strip_prefix(PREFIX) {
            let index = suffix
                .parse::<u64>()
                .map_err(|_| invalid("invalid segment name"))?;
            if index == 0
                || name != format!("{PREFIX}{index:06}")
                || component != Some(SEGMENT_COMPONENT)
            {
                return Err(invalid("segment name or component mismatch"));
            }
            Some(index)
        } else {
            if component == Some(SEGMENT_COMPONENT) {
                return Err(invalid("segment label has an unexpected name"));
            }
            None
        };
        if let Some(index) = index {
            validate_identity(&map, namespace)?;
            if map
                .metadata
                .owner_references
                .as_ref()
                .is_some_and(|owners| !owners.is_empty())
            {
                return Err(invalid("receipt segment has a foreign owner"));
            }
            if segments.insert(index, map).is_some() {
                return Err(invalid("duplicate segment"));
            }
        } else {
            let destination = match name {
                "kars-receipt-checkpoint" => &mut log.checkpoint,
                "kars-receipt-witness" => &mut log.witness,
                "kars-receipt-pubkey" => &mut log.public_key,
                _ => continue,
            };
            validate_identity(&map, namespace)?;
            if destination.is_some() {
                return Err(invalid("duplicate receipt artifact"));
            }
            *destination = Some(
                map.data
                    .ok_or_else(|| invalid("receipt artifact has no data"))?,
            );
        }
    }
    let mut previous = "genesis".to_string();
    for (position, (index, map)) in segments.into_iter().enumerate() {
        if index != position as u64 {
            return Err(invalid("missing head or segment gap"));
        }
        let data = map
            .data
            .ok_or_else(|| invalid("receipt segment has no data"))?;
        let raw = data
            .get("chain.json")
            .ok_or_else(|| invalid("receipt segment has no chain.json"))?;
        let entries: Vec<ChainEntry> =
            serde_json::from_str(raw).map_err(|_| invalid("malformed chain.json"))?;
        if index > 0
            && (entries.is_empty()
                || data.get("segmentIndex") != Some(&index.to_string())
                || data.get("previousRootHash") != Some(&previous))
        {
            return Err(invalid("empty segment or mismatched index/previous root"));
        }
        for entry in entries {
            if entry.seq != log.entries.len() as i64
                || entry.prev_hash != previous
                || chain_entry_hash(
                    entry.seq,
                    &entry.receipt,
                    &entry.payload_sha256,
                    &entry.prev_hash,
                ) != entry.entry_hash
            {
                return Err(invalid("noncontiguous or corrupt entry chain"));
            }
            previous = entry.entry_hash.clone();
            log.entries.push(entry);
        }
        log.present = true;
    }
    if !log.present
        && log.checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.get("treeSize").map(String::as_str) != Some("0")
                || checkpoint.get("rootHash").map(String::as_str) != Some("genesis")
        })
    {
        return Err(invalid("checkpoint refers to a missing log"));
    }
    Ok(log)
}

impl Cluster {
    pub(crate) async fn receipt_log(&self) -> Result<ReceiptLog, ReceiptLogError> {
        self.receipt_log_in(&self.core_namespace()).await
    }

    async fn receipt_log_in(&self, namespace: &str) -> Result<ReceiptLog, ReceiptLogError> {
        let maps: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let request = kube::core::Request::new(maps.resource_url())
            .list(&ListParams::default())
            .map_err(|_| ReceiptLogError::Read)?;
        // kube's JSON request helper logs malformed bodies and normalizes null
        // list items to empty. Preserve authentication, but validate the raw reply here.
        let response = self
            .client
            .send(request.map(kube::client::Body::from))
            .await
            .map_err(|_| ReceiptLogError::Read)?;
        if response.status() != http::StatusCode::OK {
            return Err(ReceiptLogError::Api(response.status().as_u16()));
        }
        let bytes = axum::body::to_bytes(axum::body::Body::new(response.into_body()), usize::MAX)
            .await
            .map_err(|_| ReceiptLogError::Read)?;
        let snapshot =
            serde_json::from_slice(&bytes).map_err(|_| invalid("malformed API snapshot"))?;
        parse_snapshot(snapshot, namespace)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod digest_tests {
    use super::*;

    #[test]
    fn chain_hash_keeps_decimal_sequence_and_exact_pipe_framing() {
        assert_eq!(
            chain_entry_hash(0, "team/task", "abc", ""),
            "aaf7650dad15d6e904ca99871f8bd152e6495e8bfd9e86a42bfd39f10a7c8499"
        );
        assert_ne!(
            chain_entry_hash(0, "team/task", "abc", ""),
            chain_entry_hash(0, "team/task", "abc", "\n")
        );
    }
}
