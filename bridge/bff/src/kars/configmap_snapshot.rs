// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Filter a complete API snapshot while decoding, without buffering unrelated history.

use std::fmt;
use std::io::BufReader;

use axum::body::Body;
use k8s_openapi::api::core::v1::ConfigMap;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ListMeta;
use kube::{Api, api::ListParams};
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use tokio_stream::StreamExt;
use tokio_util::io::{StreamReader, SyncIoBridge};

use super::cluster::Cluster;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConfigMapSnapshot<T> {
    pub api_version: String,
    pub kind: String,
    pub metadata: ListMeta,
    pub items: Vec<T>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SnapshotError {
    #[error("ConfigMap snapshot API status {0}")]
    Api(u16),
    #[error("ConfigMap snapshot transport or decoding failed")]
    Read,
}

struct SnapshotSeed<'a, F> {
    namespace: &'a str,
    select: F,
}

struct ItemsSeed<'a, F> {
    namespace: &'a str,
    select: &'a mut F,
}

impl<'de, T, F> DeserializeSeed<'de> for ItemsSeed<'_, F>
where
    F: FnMut(ConfigMap) -> Option<T>,
{
    type Value = Vec<T>;

    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de, T, F> Visitor<'de> for ItemsSeed<'_, F>
where
    F: FnMut(ConfigMap) -> Option<T>,
{
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a ConfigMap array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut selected = Vec::new();
        while let Some(map) = sequence.next_element::<ConfigMap>()? {
            if map.metadata.namespace.as_deref() != Some(self.namespace)
                || map.metadata.name.as_deref().is_none_or(str::is_empty)
            {
                return Err(de::Error::custom("foreign or unnamed ConfigMap"));
            }
            if let Some(value) = (self.select)(map) {
                selected.push(value);
            }
        }
        Ok(selected)
    }
}

impl<'de, T, F> DeserializeSeed<'de> for SnapshotSeed<'_, F>
where
    F: FnMut(ConfigMap) -> Option<T>,
{
    type Value = ConfigMapSnapshot<T>;

    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de, T, F> Visitor<'de> for SnapshotSeed<'_, F>
where
    F: FnMut(ConfigMap) -> Option<T>,
{
    type Value = ConfigMapSnapshot<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a complete ConfigMapList snapshot")
    }

    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut api_version = None;
        let mut kind = None;
        let mut metadata: Option<ListMeta> = None;
        let mut items = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "apiVersion" if api_version.is_none() => api_version = Some(map.next_value()?),
                "kind" if kind.is_none() => kind = Some(map.next_value()?),
                "metadata" if metadata.is_none() => metadata = Some(map.next_value()?),
                "items" if items.is_none() => {
                    items = Some(map.next_value_seed(ItemsSeed {
                        namespace: self.namespace,
                        select: &mut self.select,
                    })?);
                }
                "apiVersion" | "kind" | "metadata" | "items" => {
                    return Err(de::Error::custom("duplicate snapshot field"));
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        let snapshot = ConfigMapSnapshot {
            api_version: api_version.ok_or_else(|| de::Error::missing_field("apiVersion"))?,
            kind: kind.ok_or_else(|| de::Error::missing_field("kind"))?,
            metadata: metadata.ok_or_else(|| de::Error::missing_field("metadata"))?,
            items: items.ok_or_else(|| de::Error::missing_field("items"))?,
        };
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
            return Err(de::Error::custom("incomplete or foreign API snapshot"));
        }
        Ok(snapshot)
    }
}

async fn decode_snapshot<T, F>(
    body: Body,
    namespace: String,
    select: F,
) -> Result<ConfigMapSnapshot<T>, SnapshotError>
where
    T: Send + 'static,
    F: FnMut(ConfigMap) -> Option<T> + Send + 'static,
{
    let stream = body
        .into_data_stream()
        .map(|chunk| chunk.map_err(std::io::Error::other));
    let reader = SyncIoBridge::new(StreamReader::new(stream));
    // serde's synchronous visitor runs off the async executor. The reader and
    // one ConfigMap are bounded; only selected evidence survives each item.
    tokio::task::spawn_blocking(move || {
        let mut decoder = serde_json::Deserializer::from_reader(BufReader::new(reader));
        let snapshot = SnapshotSeed {
            namespace: &namespace,
            select,
        }
        .deserialize(&mut decoder)
        .map_err(|_| SnapshotError::Read)?;
        decoder.end().map_err(|_| SnapshotError::Read)?;
        Ok(snapshot)
    })
    .await
    .map_err(|_| SnapshotError::Read)?
}

impl Cluster {
    pub(crate) async fn configmap_snapshot<T, F>(
        &self,
        namespace: &str,
        select: F,
    ) -> Result<ConfigMapSnapshot<T>, SnapshotError>
    where
        T: Send + 'static,
        F: FnMut(ConfigMap) -> Option<T> + Send + 'static,
    {
        let maps: Api<ConfigMap> = Api::namespaced(self.client.clone(), namespace);
        let request = kube::core::Request::new(maps.resource_url())
            .list(&ListParams::default())
            .map_err(|_| SnapshotError::Read)?;
        let response = self
            .client
            .send(request.map(kube::client::Body::from))
            .await
            .map_err(|_| SnapshotError::Read)?;
        if response.status() != http::StatusCode::OK {
            return Err(SnapshotError::Api(response.status().as_u16()));
        }
        decode_snapshot(
            Body::new(response.into_body()),
            namespace.to_owned(),
            select,
        )
        .await
    }
}

#[cfg(test)]
mod tests;
