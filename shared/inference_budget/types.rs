// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const CONTRACT_VERSION: &str = "governed-inference/v1";
pub const MAX_NODES: usize = 128;
pub const MAX_SESSIONS: usize = 256;
pub const MAX_ATTEMPTS: usize = 256;
pub const MAX_REPLAY_WINDOW: u64 = 64;
pub const MAX_LEDGER_BYTES: usize = 524_288;
pub const RESERVATION_TTL_SECONDS: i64 = 30;
pub const MAX_LEDGER_INTEGER: u64 = i64::MAX as u64;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum BudgetScope {
    #[default]
    GovernedInference,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum RootKind {
    KarsTask,
    KarsTeam,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceIdentity {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

impl ResourceIdentity {
    pub fn validate(&self) -> Result<(), BudgetError> {
        if !valid_label(&self.namespace) || !valid_name(&self.name) || !valid_uid(&self.uid) {
            return Err(BudgetError::Identity);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RootIdentity {
    pub kind: RootKind,
    pub resource: ResourceIdentity,
    pub workspace_uid: String,
    pub cluster_uid: String,
}

impl RootIdentity {
    pub fn validate(&self) -> Result<(), BudgetError> {
        self.resource.validate()?;
        if !valid_uid(&self.workspace_uid) || !valid_uid(&self.cluster_uid) {
            return Err(BudgetError::Identity);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_micros: Option<u64>,
}

impl Limits {
    pub fn normalized(self) -> Self {
        Self {
            tokens: self.tokens.filter(|n| *n > 0),
            usd_micros: self.usd_micros.filter(|n| *n > 0),
        }
    }

    pub fn finite(self) -> bool {
        let normalized = self.normalized();
        normalized.tokens.is_some() || normalized.usd_micros.is_some()
    }

    pub fn allows(self, amounts: Amounts) -> bool {
        let limits = self.normalized();
        limits.tokens.is_none_or(|limit| amounts.tokens <= limit)
            && limits
                .usd_micros
                .is_none_or(|limit| amounts.usd_micros <= limit)
    }

    pub fn attenuates(self, parent: Self) -> bool {
        let child = self.normalized();
        let parent = parent.normalized();
        parent
            .tokens
            .is_none_or(|limit| child.tokens.is_some_and(|value| value <= limit))
            && parent
                .usd_micros
                .is_none_or(|limit| child.usd_micros.is_some_and(|value| value <= limit))
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Amounts {
    pub tokens: u64,
    /// Maximum configured inference-price units, not an invoice or all-in cost.
    pub usd_micros: u64,
}

impl Amounts {
    pub fn add(self, other: Self) -> Result<Self, BudgetError> {
        let amount = Self {
            tokens: self
                .tokens
                .checked_add(other.tokens)
                .ok_or(BudgetError::Overflow)?,
            usd_micros: self
                .usd_micros
                .checked_add(other.usd_micros)
                .ok_or(BudgetError::Overflow)?,
        };
        if amount.tokens > MAX_LEDGER_INTEGER || amount.usd_micros > MAX_LEDGER_INTEGER {
            return Err(BudgetError::Overflow);
        }
        Ok(amount)
    }

    pub fn subtract(self, other: Self) -> Result<Self, BudgetError> {
        Ok(Self {
            tokens: self
                .tokens
                .checked_sub(other.tokens)
                .ok_or(BudgetError::Corrupt)?,
            usd_micros: self
                .usd_micros
                .checked_sub(other.usd_micros)
                .ok_or(BudgetError::Corrupt)?,
        })
    }

    pub fn within(self, upper: Self) -> bool {
        self.tokens <= upper.tokens && self.usd_micros <= upper.usd_micros
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskAuthority {
    pub task: ResourceIdentity,
    pub parent_uid: Option<String>,
    pub root_task_uid: String,
    pub authorization_digest: String,
    /// Full effective snapshot supplied by the controller's authorization helper.
    pub effective_authorization: serde_json::Value,
    pub limits: Limits,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionIdentity {
    pub task_uid: String,
    pub authorization_digest: String,
    pub sandbox: ResourceIdentity,
    pub runtime_namespace_uid: String,
    pub pod_name: String,
    pub pod_uid: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountReference {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskBudgetBinding {
    pub scope: BudgetScope,
    pub account: AccountReference,
    pub root: RootIdentity,
    pub task_uid: String,
    pub parent_task_uid: Option<String>,
    pub root_task_uid: String,
    pub authorization_digest: String,
}

/// Controller-authored router configuration. Pod identity is supplied through
/// the downward API, not an agent request header. This object contains no token.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouterBinding {
    pub task: TaskBudgetBinding,
    pub sandbox: ResourceIdentity,
    pub runtime_namespace: String,
    pub runtime_namespace_uid: String,
    pub privacy_epoch: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrokerRequest<T> {
    pub root: RootIdentity,
    pub payload: T,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRequest {
    pub account_uid: String,
    pub identity: ExecutionIdentity,
}

impl ExecutionIdentity {
    pub fn validate(&self) -> Result<(), BudgetError> {
        self.sandbox.validate()?;
        if !valid_uid(&self.task_uid)
            || !valid_uid(&self.runtime_namespace_uid)
            || !valid_name(&self.pod_name)
            || !valid_uid(&self.pod_uid)
            || !valid_digest(&self.authorization_digest)
        {
            return Err(BudgetError::Identity);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum AccountPhase {
    Active,
    Closing,
    Closed,
    Frozen,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum AttemptPhase {
    Reserved,
    InFlight,
    Settled,
    Uncertain,
    Expired,
}

impl AttemptPhase {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Settled | Self::Uncertain | Self::Expired)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptKey {
    pub pod_uid: String,
    pub sequence: u64,
}

impl AttemptKey {
    pub fn storage_key(&self) -> String {
        format!("{}:{}", self.pod_uid, self.sequence)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReserveRequest {
    pub account_uid: String,
    pub identity: ExecutionIdentity,
    pub sequence: u64,
    pub wire_digest: String,
    pub quote: super::tariffs::Quote,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptCommand {
    pub account_uid: String,
    pub key: AttemptKey,
    pub identity: ExecutionIdentity,
    pub wire_digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub reasoning_output_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settlement {
    pub attempt: AttemptCommand,
    /// Absent/incomplete/malformed upstream usage commits the entire bound.
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum BudgetError {
    #[error("Governed inference budget identity is invalid or changed")]
    Identity,
    #[error("Governed inference authorization is missing, stale, or amplified")]
    Authorization,
    #[error("Governed inference account is not accepting dispatch")]
    Closed,
    #[error("Governed inference budget capacity is exhausted")]
    Capacity,
    #[error("Governed inference budget ceiling would be exceeded")]
    Exhausted,
    #[error("Governed inference budget arithmetic overflow")]
    Overflow,
    #[error("Governed inference ledger is corrupt; it must not reset to zero")]
    Corrupt,
    #[error("Governed inference sequence is out of order or already retired")]
    Sequence,
    #[error("Governed inference attempt is already dispatched or finalized; no re-dispatch")]
    AlreadyDispatched,
    #[error("Governed inference reservation expired before dispatch")]
    Expired,
    #[error("Governed inference contract is missing, expired, invalid, or unsupported")]
    Contract,
    #[error("Governed inference usage exceeded its reserved contract bound")]
    Breach,
}

pub fn valid_uid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}

pub fn valid_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 253 && value.split('.').all(valid_label)
}

pub fn valid_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}
