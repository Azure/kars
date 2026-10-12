// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    pub namespace: String,
    pub task_name: String,
    pub task_uid: String,
    pub sandbox_uid: String,
    pub pod_uid: String,
    pub run_nonce: String,
    pub assignment_id: String,
    pub agent_did: String,
    pub artifact_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub name: String,
    pub source: Source,
    pub sha256: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunArchive {
    pub version: u8,
    pub output: String,
    pub output_sha256: String,
    pub documents: Vec<Document>,
    pub handoffs: Vec<Document>,
}

pub fn archive_key(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("archive-{safe}")
}

pub fn digest(content: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(content.as_bytes()))
}

impl RunArchive {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1
                && !self.output.trim().is_empty()
                && self.output_sha256 == digest(&self.output)
                && serde_json::to_vec(self)?.len() <= 256 * 1024,
            "invalid or oversized retained run archive"
        );
        for documents in [&self.documents, &self.handoffs] {
            ensure!(
                !documents.is_empty() && documents.len() <= 16,
                "invalid archive document count"
            );
            let mut names = BTreeSet::new();
            for document in documents {
                let source = &document.source;
                ensure!(
                    !document.name.is_empty()
                        && document.name.len() <= 128
                        && document.name.as_bytes()[0].is_ascii_alphanumeric()
                        && document
                            .name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                        && names.insert(&document.name)
                        && !document.content.trim().is_empty()
                        && document.content.len() <= 65536
                        && document.sha256 == digest(&document.content)
                        && [
                            &source.namespace,
                            &source.task_name,
                            &source.task_uid,
                            &source.sandbox_uid,
                            &source.pod_uid,
                            &source.run_nonce,
                            &source.assignment_id,
                            &source.agent_did,
                            &source.artifact_name,
                        ]
                        .iter()
                        .all(|s| !s.trim().is_empty() && s.len() <= 253),
                    "retained document content or provenance is invalid"
                );
            }
        }
        ensure!(
            serde_json::to_vec(&self.handoffs)?.len() <= 65536,
            "retained handoffs exceed the installed input limit"
        );
        let root = &self.documents[0].source;
        ensure!(
            self.documents.iter().all(|document| {
                let source = &document.source;
                source.namespace == root.namespace
                    && source.task_name == root.task_name
                    && source.task_uid == root.task_uid
                    && source.run_nonce == root.run_nonce
                    && source.assignment_id == root.assignment_id
                    && source.sandbox_uid == root.sandbox_uid
                    && source.pod_uid == root.pod_uid
                    && source.agent_did == root.agent_did
                    && source.artifact_name == document.name
            }) && self
                .handoffs
                .iter()
                .all(|document| document.source.namespace == root.namespace),
            "retained archive mixes principal identities or namespaces"
        );
        Ok(())
    }
}
