// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Immutable, account-bound radix history. Only the root committed in the
//! account status is authoritative; prepared objects never authorize a grant.

use super::{StoreError, api_error};
use crate::inference_budget_contract::{
    BudgetError, MAX_NODES,
    ledger::{HistoryRecord, Ledger},
    valid_digest, valid_uid,
};
use crate::providers::signing::sha256_hex;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, ResourceExt, api::PostParams};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
};

const MAX_PAGE_BYTES: usize = 700 * 1024;
const COMPONENT: &str = "kars-budget-history";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Summary {
    nodes: u64,
    sessions: u64,
    unknown: u64,
}

impl Summary {
    fn add(&mut self, other: &Self) -> Result<(), StoreError> {
        self.nodes = self
            .nodes
            .checked_add(other.nodes)
            .ok_or(BudgetError::Overflow)?;
        self.sessions = self
            .sessions
            .checked_add(other.sessions)
            .ok_or(BudgetError::Overflow)?;
        self.unknown = self
            .unknown
            .checked_add(other.unknown)
            .ok_or(BudgetError::Overflow)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Link {
    hash: String,
    summary: Summary,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Node {
    Leaf {
        key: String,
        record: HistoryRecord,
    },
    Branch {
        prefix: String,
        children: BTreeMap<char, Link>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Page {
    version: u32,
    account_uid: String,
    node: Node,
    summary: Summary,
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

fn route(key: &str) -> String {
    sha256_hex(key.as_bytes())
}

fn name(hash: &str) -> String {
    format!("{COMPONENT}-{}", &hash[7..])
}

fn summary(node: &Node) -> Result<Summary, StoreError> {
    let mut value = Summary::default();
    match node {
        Node::Leaf { key, record } => {
            if record.key() != *key {
                return Err(BudgetError::Corrupt.into());
            }
            match record {
                HistoryRecord::Node {
                    node,
                    unknown_usage_attempts,
                } => {
                    if node.active
                        || !valid_uid(&node.authority.task.uid)
                        || *unknown_usage_attempts != node.meters.unknown_usage_attempts
                    {
                        return Err(BudgetError::Corrupt.into());
                    }
                    value.nodes = 1;
                }
                HistoryRecord::Session { session, attempts } => {
                    if !session.closed
                        || !valid_uid(&session.identity.pod_uid)
                        || attempts.values().any(|attempt| !attempt.phase.terminal())
                    {
                        return Err(BudgetError::Corrupt.into());
                    }
                    value.sessions = 1;
                    value.unknown = record.unknown();
                }
            }
        }
        Node::Branch { prefix, children } => {
            if prefix.len() >= 64
                || !prefix
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                || !(2..=16).contains(&children.len())
            {
                return Err(BudgetError::Corrupt.into());
            }
            for (digit, link) in children {
                if !digit.is_ascii_hexdigit()
                    || digit.is_ascii_uppercase()
                    || !valid_digest(&link.hash)
                {
                    return Err(BudgetError::Corrupt.into());
                }
                value.add(&link.summary)?;
            }
        }
    }
    Ok(value)
}

pub(super) struct Tree {
    api: Api<ConfigMap>,
    namespace: String,
    account_uid: String,
    root: Option<String>,
    cache: BTreeMap<String, Page>,
    pending: BTreeSet<String>,
}

impl Tree {
    pub(super) fn new(api: Api<ConfigMap>, namespace: &str, ledger: &Ledger) -> Self {
        Self {
            api,
            namespace: namespace.into(),
            account_uid: ledger.account_uid.clone(),
            root: ledger.history.as_ref().map(|history| history.root.clone()),
            cache: BTreeMap::new(),
            pending: BTreeSet::new(),
        }
    }

    fn decode(&self, object: &ConfigMap, hash: &str) -> Result<Page, StoreError> {
        if !valid_digest(hash)
            || object.metadata.name.as_deref() != Some(name(hash).as_str())
            || object.namespace().as_deref() != Some(self.namespace.as_str())
            || object.uid().as_deref().is_none_or(str::is_empty)
            || object
                .resource_version()
                .as_deref()
                .is_none_or(str::is_empty)
            || object.metadata.deletion_timestamp.is_some()
            || object.immutable != Some(true)
            || !object.owner_references().is_empty()
            || object
                .labels()
                .get("app.kubernetes.io/component")
                .map(String::as_str)
                != Some(COMPONENT)
            || object
                .annotations()
                .get("kars.azure.com/account-uid")
                .map(String::as_str)
                != Some(self.account_uid.as_str())
            || object
                .binary_data
                .as_ref()
                .is_some_and(|data| !data.is_empty())
        {
            return Err(BudgetError::Corrupt.into());
        }
        let data = object.data.as_ref().ok_or(BudgetError::Corrupt)?;
        let body = data.get("history.json").ok_or(BudgetError::Corrupt)?;
        if data.len() != 1 || body.len() > MAX_PAGE_BYTES || digest(body.as_bytes()) != hash {
            return Err(BudgetError::Corrupt.into());
        }
        let page: Page = serde_json::from_str(body).map_err(|_| BudgetError::Corrupt)?;
        if page.version != 1
            || page.account_uid != self.account_uid
            || summary(&page.node)? != page.summary
        {
            return Err(BudgetError::Corrupt.into());
        }
        Ok(page)
    }

    async fn get(&mut self, hash: &str) -> Result<Page, StoreError> {
        if !valid_digest(hash) {
            return Err(BudgetError::Corrupt.into());
        }
        if let Some(page) = self.cache.get(hash) {
            return Ok(page.clone());
        }
        let object = self
            .api
            .get(&name(hash))
            .await
            .map_err(|e| api_error("read accounting history", e))?;
        let page = self.decode(&object, hash)?;
        self.cache.insert(hash.into(), page.clone());
        Ok(page)
    }

    fn put(&mut self, node: Node) -> Result<Link, StoreError> {
        let page = Page {
            version: 1,
            account_uid: self.account_uid.clone(),
            summary: summary(&node)?,
            node,
        };
        let body = serde_json::to_vec(&page).map_err(|_| BudgetError::Corrupt)?;
        if body.len() > MAX_PAGE_BYTES {
            return Err(BudgetError::Capacity.into());
        }
        let hash = digest(&body);
        let link = Link {
            hash: hash.clone(),
            summary: page.summary.clone(),
        };
        if !self.cache.contains_key(&hash) {
            self.pending.insert(hash.clone());
            self.cache.insert(hash, page);
        }
        Ok(link)
    }

    async fn root_link(&mut self) -> Result<Option<Link>, StoreError> {
        match self.root.clone() {
            None => Ok(None),
            Some(hash) => Ok(Some(Link {
                summary: self.get(&hash).await?.summary,
                hash,
            })),
        }
    }

    pub(super) async fn verify(&mut self, ledger: &Ledger) -> Result<(), StoreError> {
        let observed = self
            .root_link()
            .await?
            .map(|link| link.summary)
            .unwrap_or_default();
        let expected = ledger
            .history
            .as_ref()
            .map(|history| Summary {
                nodes: history.nodes,
                sessions: history.sessions,
                unknown: history.unknown_usage_attempts,
            })
            .unwrap_or_default();
        if observed != expected {
            return Err(BudgetError::Corrupt.into());
        }
        Ok(())
    }

    async fn bind(&mut self, ledger: &mut Ledger) -> Result<(), StoreError> {
        if let Some(root) = &self.root {
            ledger.history.as_mut().ok_or(BudgetError::Corrupt)?.root = root.clone();
        } else if let Some(history) = &ledger.history {
            if history.nodes != 0
                || history.sessions != 0
                || history.unknown_usage_attempts != 0
                || !history.node_unknown.is_empty()
            {
                return Err(BudgetError::Corrupt.into());
            }
            ledger.history = None;
        }
        self.verify(ledger).await
    }

    async fn lookup(&mut self, key: &str) -> Result<Option<HistoryRecord>, StoreError> {
        let path = route(key);
        let mut next = self.root_link().await?;
        let mut required = String::new();
        for _ in 0..=64 {
            let Some(link) = next else {
                return Ok(None);
            };
            let page = self.get(&link.hash).await?;
            if page.summary != link.summary {
                return Err(BudgetError::Corrupt.into());
            }
            match page.node {
                Node::Leaf {
                    key: stored,
                    record,
                } => {
                    if !route(&stored).starts_with(&required) {
                        return Err(BudgetError::Corrupt.into());
                    }
                    return Ok((stored == key).then_some(record));
                }
                Node::Branch { prefix, children } => {
                    if !prefix.starts_with(&required) {
                        return Err(BudgetError::Corrupt.into());
                    }
                    if !path.starts_with(&prefix) {
                        return Ok(None);
                    }
                    let digit = path.as_bytes()[prefix.len()] as char;
                    required = format!("{prefix}{digit}");
                    next = children.get(&digit).cloned();
                }
            }
        }
        Err(BudgetError::Corrupt.into())
    }

    fn change<'a>(
        &'a mut self,
        current: Option<Link>,
        key: &'a str,
        record: Option<HistoryRecord>,
        depth: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Link>, StoreError>> + Send + 'a>> {
        Box::pin(async move {
            if depth > 64 {
                return Err(BudgetError::Corrupt.into());
            }
            let Some(link) = current else {
                return record
                    .map(|record| {
                        self.put(Node::Leaf {
                            key: key.into(),
                            record,
                        })
                    })
                    .transpose();
            };
            let page = self.get(&link.hash).await?;
            if page.summary != link.summary {
                return Err(BudgetError::Corrupt.into());
            }
            let path = route(key);
            let old_path = match &page.node {
                Node::Leaf { key: old, .. } if old == key => {
                    return record
                        .map(|record| {
                            self.put(Node::Leaf {
                                key: key.into(),
                                record,
                            })
                        })
                        .transpose();
                }
                Node::Leaf { key, .. } => route(key),
                Node::Branch { prefix, .. } => prefix.clone(),
            };
            let common = path
                .bytes()
                .zip(old_path.bytes())
                .take_while(|(a, b)| a == b)
                .count();
            if let Node::Branch {
                prefix,
                mut children,
            } = page.node
            {
                if common == prefix.len() {
                    let digit = path.as_bytes()[prefix.len()] as char;
                    let child = children.remove(&digit);
                    if let Some(changed) = self.change(child, key, record, depth + 1).await? {
                        children.insert(digit, changed);
                    }
                    return match children.len() {
                        0 => Ok(None),
                        1 => Ok(children.into_values().next()),
                        _ => self.put(Node::Branch { prefix, children }).map(Some),
                    };
                }
            }
            let Some(record) = record else {
                return Ok(Some(link));
            };
            if common >= 64 || common >= old_path.len() {
                return Err(BudgetError::Corrupt.into());
            }
            let leaf = self.put(Node::Leaf {
                key: key.into(),
                record,
            })?;
            let children = BTreeMap::from([
                (old_path.as_bytes()[common] as char, link),
                (path.as_bytes()[common] as char, leaf),
            ]);
            self.put(Node::Branch {
                prefix: path[..common].into(),
                children,
            })
            .map(Some)
        })
    }

    async fn set(&mut self, key: &str, record: Option<HistoryRecord>) -> Result<(), StoreError> {
        let root = self.root_link().await?;
        self.root = self
            .change(root, key, record, 0)
            .await?
            .map(|link| link.hash);
        Ok(())
    }

    pub(super) async fn archive(
        &mut self,
        ledger: &mut Ledger,
        protected: &BTreeSet<String>,
    ) -> Result<bool, StoreError> {
        let records = ledger.archive_closed(protected)?;
        let changed = !records.is_empty();
        for record in records {
            let key = record.key();
            if self.lookup(&key).await?.is_some() {
                return Err(BudgetError::Corrupt.into());
            }
            self.set(&key, Some(record)).await?;
        }
        self.bind(ledger).await?;
        Ok(changed)
    }

    pub(super) async fn hydrate(
        &mut self,
        ledger: &mut Ledger,
        requested: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, StoreError> {
        self.verify(ledger).await?;
        let mut protected = requested.clone();
        let mut queue: Vec<_> = requested.iter().cloned().collect();
        let mut records = BTreeMap::new();
        while let Some(key) = queue.pop() {
            let present = if let Some(uid) = key.strip_prefix("node:") {
                if let Some(node) = ledger.nodes.get(uid) {
                    if let Some(parent) = &node.authority.parent_uid {
                        let parent = HistoryRecord::node_key(parent);
                        if protected.insert(parent.clone()) {
                            queue.push(parent);
                        }
                    }
                    true
                } else {
                    false
                }
            } else if let Some(uid) = key.strip_prefix("session:") {
                ledger.sessions.contains_key(uid)
            } else {
                return Err(BudgetError::Identity.into());
            };
            if present {
                continue;
            }
            let Some(record) = self.lookup(&key).await? else {
                continue;
            };
            let parent = match &record {
                HistoryRecord::Node { node, .. } => node.authority.parent_uid.clone(),
                HistoryRecord::Session { session, .. } => Some(session.identity.task_uid.clone()),
            };
            if let Some(parent) = parent {
                let parent = HistoryRecord::node_key(&parent);
                if protected.insert(parent.clone()) {
                    queue.push(parent);
                }
            }
            records.insert(key, record);
            if records.len() > MAX_NODES + crate::inference_budget_contract::MAX_SESSIONS {
                return Err(BudgetError::Capacity.into());
            }
        }
        while !records.is_empty() {
            let ready = records
                .iter()
                .find(|(_, record)| match record {
                    HistoryRecord::Node { node, .. } => node
                        .authority
                        .parent_uid
                        .as_ref()
                        .is_none_or(|parent| ledger.nodes.contains_key(parent)),
                    HistoryRecord::Session { session, .. } => {
                        ledger.nodes.contains_key(&session.identity.task_uid)
                    }
                })
                .map(|(key, _)| key.clone())
                .ok_or(BudgetError::Corrupt)?;
            let record = records.remove(&ready).ok_or(BudgetError::Corrupt)?;
            loop {
                match ledger.restore_history(&record) {
                    Ok(()) => break,
                    Err(BudgetError::Capacity) if self.archive(ledger, &protected).await? => {}
                    Err(error) => return Err(error.into()),
                }
            }
            self.set(&ready, None).await?;
            self.bind(ledger).await?;
        }
        loop {
            match ledger.validate() {
                Ok(()) => return Ok(protected),
                Err(BudgetError::Capacity) if self.archive(ledger, &protected).await? => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// All referenced pages are durable before the single account-root CAS.
    /// Conflicts/crashes may leave unreachable immutable pages, never a partial
    /// authoritative history. No deletion/GC races with historical readers.
    pub(super) async fn flush(&mut self) -> Result<(), StoreError> {
        for hash in self.pending.clone() {
            let page = self.cache.get(&hash).ok_or(BudgetError::Corrupt)?.clone();
            let body = serde_json::to_string(&page).map_err(|_| BudgetError::Corrupt)?;
            let object = ConfigMap {
                metadata: kube::api::ObjectMeta {
                    name: Some(name(&hash)),
                    namespace: Some(self.namespace.clone()),
                    labels: Some(BTreeMap::from([(
                        "app.kubernetes.io/component".into(),
                        COMPONENT.into(),
                    )])),
                    annotations: Some(BTreeMap::from([(
                        "kars.azure.com/account-uid".into(),
                        self.account_uid.clone(),
                    )])),
                    ..Default::default()
                },
                immutable: Some(true),
                data: Some(BTreeMap::from([("history.json".into(), body)])),
                ..Default::default()
            };
            let stored = match self.api.create(&PostParams::default(), &object).await {
                Ok(stored) => stored,
                Err(kube::Error::Api(error)) if error.code == 409 => self
                    .api
                    .get(&name(&hash))
                    .await
                    .map_err(|e| api_error("verify existing accounting history", e))?,
                Err(error) => return Err(api_error("prepare accounting history", error)),
            };
            if self.decode(&stored, &hash)? != page {
                return Err(BudgetError::Corrupt.into());
            }
            self.pending.remove(&hash);
        }
        Ok(())
    }
}
