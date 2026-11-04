// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Private lexical memory. Deliberately separate from approved Team knowledge.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const MAX_BYTES: usize = 900 * 1024;
pub const MAX_RECORDS: usize = 512;

/// Must match the installed guard before the controller mints private tokens.
pub fn account_policy(controller_namespace: &str) -> serde_json::Value {
    serde_json::json!({
        "failurePolicy":"Fail",
        "matchConstraints": {"matchPolicy":"Equivalent", "namespaceSelector":{}, "objectSelector":{},
            "resourceRules":[{"apiGroups":[""],"apiVersions":["v1"],"operations":["CREATE","UPDATE","CONNECT"],"resources":["pods","pods/*","serviceaccounts/token"],"scope":"Namespaced"}]},
        "matchConditions":[{"name":"private-memory-namespace","expression":"request.namespace == 'kars-memory-system'"}],
        "validations":[{"expression":format!("request.resource.resource != 'pods' && request.userInfo.username == 'system:serviceaccount:{controller_namespace}:kars-controller'"),"message":"Private memory accounts cannot run Pods; only the core controller may mint their tokens","reason":"Forbidden"}]
    })
}

pub fn digest(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub namespace: String,
    pub namespace_uid: String,
    pub owner_kind: String,
    pub owner_name: String,
    pub owner_uid: String,
    pub principal: String,
}

impl Identity {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(
                self.owner_kind.as_str(),
                "KarsTeam" | "KarsTask" | "KarsSandbox"
            ),
            "invalid memory owner kind"
        );
        ensure!(
            [
                &self.namespace,
                &self.namespace_uid,
                &self.owner_name,
                &self.owner_uid,
                &self.principal
            ]
            .iter()
            .all(|s| !s.is_empty() && s.len() <= 253),
            "invalid memory identity"
        );
        Ok(())
    }

    pub fn key(&self) -> String {
        digest(serde_json::to_vec(self).expect("memory identity serializes"))
    }

    pub fn scope(&self) -> String {
        format!("private_{}", self.key())
    }

    pub fn map_name(&self) -> String {
        format!("kars-memory-{}", &self.key()[..48])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub memory_id: String,
    pub text: String,
    pub sha256: String,
    pub sandbox_uid: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Store {
    pub version: u8,
    pub identity: Identity,
    pub records: Vec<Record>,
}

impl Store {
    pub fn empty(identity: Identity) -> Self {
        Self {
            version: 1,
            identity,
            records: vec![],
        }
    }

    pub fn validate(&self, identity: &Identity) -> Result<()> {
        identity.validate()?;
        ensure!(
            self.version == 1 && &self.identity == identity,
            "private memory identity mismatch"
        );
        ensure!(
            self.records.len() <= MAX_RECORDS && serde_json::to_vec(self)?.len() <= MAX_BYTES,
            "private memory capacity exceeded; existing records preserved"
        );
        let mut ids = BTreeSet::new();
        for r in &self.records {
            ensure!(
                !r.text.trim().is_empty()
                    && r.text.len() <= 16384
                    && !r.sandbox_uid.is_empty()
                    && !r.updated_at.is_empty()
                    && r.sha256 == digest(&r.text)
                    && r.memory_id == r.sha256
                    && ids.insert(&r.memory_id),
                "private memory record integrity failure"
            );
        }
        Ok(())
    }

    pub fn update(&mut self, text: &str, sandbox_uid: &str, timestamp: &str) -> Result<String> {
        self.validate(&self.identity)?;
        ensure!(
            !text.trim().is_empty()
                && text.len() <= 16384
                && !sandbox_uid.is_empty()
                && !timestamp.is_empty(),
            "invalid private memory update"
        );
        let id = digest(text);
        if self.records.iter().any(|r| r.memory_id == id) {
            return Ok(id);
        }
        let mut candidate = self.clone();
        candidate.records.push(Record {
            memory_id: id.clone(),
            sha256: id.clone(),
            text: text.into(),
            sandbox_uid: sandbox_uid.into(),
            updated_at: timestamp.into(),
        });
        candidate.validate(&self.identity)?;
        *self = candidate;
        Ok(id)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<&Record> {
        let query = query.to_lowercase();
        let terms: BTreeSet<_> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| !s.is_empty())
            .collect();
        let mut matches: Vec<_> = self
            .records
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                let text = record.text.to_lowercase();
                let words: BTreeSet<_> = text
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|s| !s.is_empty())
                    .collect();
                let score = terms.intersection(&words).count();
                (terms.is_empty() || score > 0).then_some((score, index, record))
            })
            .collect();
        matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        matches
            .into_iter()
            .take(limit.min(MAX_RECORDS))
            .map(|(_, _, r)| r)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity() -> Identity {
        Identity {
            namespace: "workspace".into(),
            namespace_uid: "namespace-uid".into(),
            owner_kind: "KarsTeam".into(),
            owner_name: "team".into(),
            owner_uid: "team-uid".into(),
            principal: "researcher".into(),
        }
    }
    #[test]
    fn builtin_memory_is_private_durable_and_not_approved_knowledge() {
        let identity = identity();
        let mut store = Store::empty(identity.clone());
        let id = store
            .update(
                "The preferred city is Helsinki",
                "sandbox-a",
                "2026-10-22T00:00:00Z",
            )
            .unwrap();
        assert_eq!(
            store
                .update("The preferred city is Helsinki", "sandbox-b", "later")
                .unwrap(),
            id
        );
        let persisted = serde_json::to_vec(&store).unwrap();
        let reloaded: Store = serde_json::from_slice(&persisted).unwrap();
        reloaded.validate(&identity).unwrap();
        assert_eq!(reloaded.search("Helsinki city", 10).len(), 1);
        assert!(reloaded.search("Paris", 10).is_empty());
        assert_eq!(reloaded.records.len(), 1);
        assert!(!String::from_utf8(persisted).unwrap().contains("approved"));
    }
    #[test]
    fn builtin_memory_owner_role_namespace_and_integrity_fail_closed() {
        let identity = identity();
        let mut store = Store::empty(identity.clone());
        store.update("private fact", "sandbox-a", "now").unwrap();
        for field in ["owner_uid", "namespace_uid", "principal"] {
            let mut value = serde_json::to_value(&identity).unwrap();
            value[field] = "different".into();
            let other: Identity = serde_json::from_value(value).unwrap();
            assert_ne!(identity.scope(), other.scope());
            assert!(store.validate(&other).is_err());
        }
        store.records[0].text = "tampered".into();
        assert!(store.validate(&identity).is_err());
    }
    #[test]
    fn builtin_memory_capacity_does_not_evict_or_mutate() {
        let mut store = Store::empty(identity());
        let before = store.clone();
        assert!(store.update(&"x".repeat(16385), "sandbox", "now").is_err());
        assert_eq!(store, before);
    }
    #[test]
    fn builtin_memory_full_store_preserves_records_and_idempotence() {
        let mut store = Store::empty(identity());
        for index in 0..MAX_RECORDS {
            store
                .update(&format!("fact {index}"), "sandbox", "now")
                .unwrap();
        }
        let before = store.clone();
        assert!(store.update("one more fact", "sandbox", "later").is_err());
        assert_eq!(store, before);
        assert!(store.update("fact 0", "other-sandbox", "later").is_ok());
        assert_eq!(store, before);
    }

    #[test]
    fn builtin_memory_lexical_ranking_and_empty_recall_are_deterministic() {
        let mut store = Store::empty(identity());
        store
            .update("Coffee preference dark roast", "sandbox", "first")
            .unwrap();
        store
            .update("Coffee preference", "sandbox", "second")
            .unwrap();
        store
            .update("Coffee elsewhere", "sandbox", "third")
            .unwrap();
        assert_eq!(
            store.search("COFFEE preference!", 1)[0].text,
            "Coffee preference"
        );
        assert_eq!(store.search("", 1)[0].text, "Coffee elsewhere");
        assert!(store.search("unrelated", 10).is_empty());
    }
}
