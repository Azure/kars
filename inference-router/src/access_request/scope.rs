// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceIdentity {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub sandbox: ResourceIdentity,
    pub namespace_uid: String,
    #[serde(default)]
    pub task: Option<ResourceIdentity>,
    #[serde(default)]
    pub task_authorization: Option<String>,
    #[serde(default)]
    pub task_generation: Option<i64>,
    pub managed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Scope {
    pub id: String,
    pub identity: Identity,
    pub assignment_id: Option<String>,
}

pub fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._:/".contains(&c))
}

impl Identity {
    pub fn standalone(name: &str) -> Self {
        Self {
            sandbox: ResourceIdentity {
                namespace: "standalone".into(),
                name: name.into(),
                uid: "process-local".into(),
            },
            namespace_uid: "process-local".into(),
            task: None,
            task_authorization: None,
            task_generation: None,
            managed: false,
        }
    }
    pub fn valid(&self, sandbox_name: &str) -> bool {
        let valid = |identity: &ResourceIdentity| {
            identifier(&identity.namespace, 253)
                && identifier(&identity.name, 253)
                && identifier(&identity.uid, 128)
        };
        self.sandbox.name == sandbox_name
            && valid(&self.sandbox)
            && identifier(&self.namespace_uid, 128)
            && self
                .task
                .as_ref()
                .is_none_or(|task| valid(task) && task.namespace == self.sandbox.namespace)
            && match &self.task {
                Some(_) => {
                    self.task_generation
                        .is_some_and(|generation| generation > 0)
                        && self
                            .task_authorization
                            .as_deref()
                            .and_then(|digest| digest.strip_prefix("sha256:"))
                            .is_some_and(|digest| {
                                digest.len() == 64
                                    && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                }
                None => self.task_authorization.is_none() && self.task_generation.is_none(),
            }
    }
}
