// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{BudgetAxis, PolicyAxis};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeViolation {
    CredentialGrantNotSubset,
    GitHubGrantNotSubset,
    TierExceedsParentCeiling {
        child_tier: i32,
        parent_ceiling: i32,
    },
    CeilingExceedsParentCeiling {
        child_ceiling: i32,
        parent_ceiling: i32,
    },
    DelegationDepthExceeded {
        child_depth: i32,
        parent_depth: i32,
    },
    BudgetExceeded {
        axis: BudgetAxis,
        child: i64,
        parent: i64,
    },
    BudgetUnbounded {
        axis: BudgetAxis,
        parent: i64,
    },
    PolicyMismatch {
        axis: PolicyAxis,
        child: Option<String>,
        parent: String,
    },
    EgressNotSubset {
        host: String,
        port: Option<u16>,
    },
}

impl std::fmt::Display for EnvelopeViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CredentialGrantNotSubset => {
                write!(f, "credential sources and key grants exceed the parent")
            }
            Self::GitHubGrantNotSubset => write!(
                f,
                "GitHub connection or repository authority exceeds the parent"
            ),
            Self::TierExceedsParentCeiling {
                child_tier,
                parent_ceiling,
            } => write!(
                f,
                "tier {child_tier} exceeds parent authority ceiling {parent_ceiling}"
            ),
            Self::CeilingExceedsParentCeiling {
                child_ceiling,
                parent_ceiling,
            } => write!(
                f,
                "authorityCeiling {child_ceiling} exceeds parent authority ceiling {parent_ceiling}"
            ),
            Self::DelegationDepthExceeded {
                child_depth,
                parent_depth,
            } => write!(
                f,
                "delegationDepth {child_depth} exceeds parent budget (parent depth {parent_depth}, child must be <= {})",
                parent_depth - 1
            ),
            Self::BudgetExceeded {
                axis,
                child,
                parent,
            } => write!(f, "budget {axis:?} {child} exceeds parent cap {parent}"),
            Self::BudgetUnbounded { axis, parent } => write!(
                f,
                "budget {axis:?} is unbounded but parent caps it at {parent}"
            ),
            Self::PolicyMismatch {
                axis,
                child,
                parent,
            } => write!(
                f,
                "{axis:?} ref {} must match parent's bound `{parent}`",
                child.as_deref().unwrap_or("<none>")
            ),
            Self::EgressNotSubset { host, port } => match port {
                Some(port) => write!(
                    f,
                    "egress to {host}:{port} is not permitted by the parent (egress must be a subset of the parent's)"
                ),
                None => write!(
                    f,
                    "egress to {host} is not permitted by the parent (egress must be a subset of the parent's)"
                ),
            },
        }
    }
}
