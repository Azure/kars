// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Exact partition authorization shared by binding compilation and router enforcement.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryAccess {
    pub store_name: String,
    pub scope: String,
    #[serde(default)]
    pub shared_scopes: Vec<String>,
}

pub fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= 256
        && scope
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_./%+@-".contains(&c))
        && scope.split('/').all(|part| !matches!(part, "." | ".."))
}

impl MemoryAccess {
    pub fn validate(&self) -> Result<(), &'static str> {
        let store = self.store_name.as_bytes();
        if store.is_empty()
            || store.len() > 63
            || !store.first().is_some_and(u8::is_ascii_alphanumeric)
            || !store.last().is_some_and(u8::is_ascii_alphanumeric)
            || !store
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        {
            return Err("memory storeName must be a DNS label");
        }
        if !valid_scope(&self.scope) {
            return Err("memory bound scope is invalid");
        }
        if self.shared_scopes.len() > 64 {
            return Err("memory sharedScopes exceeds 64 grants");
        }
        for (i, scope) in self.shared_scopes.iter().enumerate() {
            if !valid_scope(scope)
                || scope == &self.scope
                || self.shared_scopes[..i].contains(scope)
            {
                return Err(
                    "memory sharedScopes must contain distinct valid scopes other than the bound scope",
                );
            }
        }
        Ok(())
    }

    pub fn authorize(&self, scope: &str) -> Result<(), &'static str> {
        self.validate()?;
        if !valid_scope(scope)
            || (scope != self.scope && !self.shared_scopes.iter().any(|s| s == scope))
        {
            return Err("memory scope is not authorized by KarsMemory");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access() -> MemoryAccess {
        MemoryAccess {
            store_name: "team-memory".into(),
            scope: "agent_alice".into(),
            shared_scopes: vec!["team_beta".into()],
        }
    }

    #[test]
    fn memory_access_exact_bound_and_shared_only() {
        let policy = access();
        assert_eq!(policy.authorize("agent_alice"), Ok(()));
        assert_eq!(policy.authorize("team_beta"), Ok(()));
        for scope in [
            "other-sandbox",
            "tenant:other-tenant",
            "system",
            "*",
            "../other",
            "",
            " team_beta",
            "team_beta ",
            "team:beta",
            "TEAM_BETA",
            "team_beta/child",
        ] {
            assert!(policy.authorize(scope).is_err(), "{scope}");
        }
    }

    #[test]
    fn memory_access_rejects_invalid_grants() {
        for scope in [
            "*",
            "../other",
            "a/../b",
            "a/./b",
            "",
            "team:beta",
            "agent_alice",
            "team_beta",
        ] {
            let mut policy = access();
            policy.shared_scopes.push(scope.into());
            assert!(policy.validate().is_err(), "{scope}");
        }
        let mut policy = access();
        policy.shared_scopes = (0..65).map(|i| format!("team_{i}")).collect();
        assert!(policy.validate().is_err());
    }

    #[test]
    fn memory_access_does_not_normalize_aliases() {
        let policy = access();
        assert!(policy.authorize("agent:alice").is_err());
        assert!(policy.authorize("%61gent_alice").is_err());
        assert!(valid_scope("team/sub-team+name@example.org"));
    }
}
