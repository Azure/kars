// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Keyless git write (design note §14) — the credential source for the router's
//! **loopback git + API reverse-proxy**.
//!
//! The router is the agent gateway: it holds the GitHub credential (a GitHub App
//! private key, or an operator-provided fine-grained PAT) and mints a short-
//! lived, repo-scoped token that it **injects on the agent's behalf**. The agent
//! pushes/clones/opens PRs through `http://127.0.0.1:8443/git/…` and `/gh-api/…`
//! on loopback and **never holds a credential** — defeating prompt-injection
//! token exfil.
//!
//! Two independent guarantees:
//!  1. **Custody** — the token lives only in the router; the agent talks plain
//!     HTTP on loopback and the router adds `Authorization`.
//!  2. **Scope** — every proxied request's `owner/repo` is checked against a
//!     fail-closed allowlist (`GIT_WRITE_REPOS`), so even a broad underlying
//!     credential can only ever reach the repositories the operator declared.

use anyhow::Result;

use crate::github_app::GitHubApp;

/// The underlying GitHub credential the router authenticates with.
enum GitCredential {
    /// A GitHub App — mints short-lived, repo-scoped installation tokens. The
    /// SOTA path (per-repo, per-permission, ~1h, revocable by uninstalling).
    App(GitHubApp),
    /// An operator-provided token (ideally a fine-grained PAT scoped to the
    /// target repos). Simpler; used when no App is configured.
    Pat(String),
}

/// Router-side git-write configuration. `None` ⇒ feature off (fail-closed).
pub struct GitWriteConfig {
    credential: GitCredential,
    /// Lowercased `owner/repo` entries the agent may reach. EMPTY ⇒ deny all
    /// (fail-closed): the operator must explicitly declare the repositories.
    allowed_repos: Vec<String>,
    /// Whether this sandbox is a principal (top-level mission) or a spawned
    /// sub-agent. Sub-agents may push branches + open PRs, but never merge — a
    /// principal (or a human via the inbox) is the merge gate.
    role: GitRole,
}

/// The role of the sandbox this router serves, for git-write authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRole {
    /// Top-level mission agent — may merge (if the workspace/envelope allows).
    Principal,
    /// Spawned sub-agent — push branches + open PRs only; never merge.
    SubAgent,
}

impl GitWriteConfig {
    /// Build from the router's environment. Returns `None` (feature off) unless
    /// a credential is configured. Precedence: GitHub App, then PAT.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let credential = if let Some(app) = GitHubApp::from_env() {
            GitCredential::App(app)
        } else {
            let pat = std::env::var("GIT_WRITE_TOKEN").ok()?;
            let pat = pat.trim().to_string();
            if pat.is_empty() {
                return None;
            }
            GitCredential::Pat(pat)
        };
        let allowed_repos = std::env::var("GIT_WRITE_REPOS")
            .ok()
            .or_else(|| std::env::var("GITHUB_APP_REPOS").ok())
            .map(|s| parse_repos(&s))
            .unwrap_or_default();
        let role = match std::env::var("KARS_GIT_ROLE").ok().as_deref() {
            Some(r) if r.trim().eq_ignore_ascii_case("subagent") => GitRole::SubAgent,
            _ => GitRole::Principal,
        };
        Some(Self { credential, allowed_repos, role })
    }

    /// The sandbox's git-write role.
    #[must_use]
    pub fn role(&self) -> GitRole {
        self.role
    }

    /// Whether this sandbox may merge a pull request. Only a principal may — a
    /// sub-agent pushes + opens PRs and asks the principal (or a human) to merge.
    #[must_use]
    pub fn can_merge(&self) -> bool {
        self.role == GitRole::Principal
    }

    /// Whether the agent may reach `owner/repo` (case-insensitive; a trailing
    /// `.git` is ignored). Fail-closed: an empty allowlist denies everything.
    #[must_use]
    pub fn repo_allowed(&self, owner_repo: &str) -> bool {
        if self.allowed_repos.is_empty() {
            return false;
        }
        let want = normalize_repo(owner_repo);
        self.allowed_repos.iter().any(|r| r == &want)
    }

    /// The set of repositories in scope (for diagnostics / the deny message).
    #[must_use]
    pub fn allowed_repos(&self) -> &[String] {
        &self.allowed_repos
    }

    /// A currently-valid token to inject. For an App this is a cached,
    /// repo-scoped installation token; for a PAT it is the token itself.
    pub async fn token(&self) -> Result<String> {
        match &self.credential {
            GitCredential::App(app) => app.installation_token().await,
            GitCredential::Pat(pat) => Ok(pat.clone()),
        }
    }
}

fn parse_repos(s: &str) -> Vec<String> {
    s.split(',')
        .map(normalize_repo)
        .filter(|r| !r.is_empty() && r.contains('/'))
        .collect()
}

/// `Owner/Repo.git` → `owner/repo`. Trims whitespace, a `.git` suffix, and any
/// surrounding slashes, and lowercases (GitHub owner/repo are case-insensitive).
fn normalize_repo(s: &str) -> String {
    s.trim()
        .trim_matches('/')
        .strip_suffix(".git")
        .unwrap_or_else(|| s.trim().trim_matches('/'))
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(repos: &[&str]) -> GitWriteConfig {
        GitWriteConfig {
            credential: GitCredential::Pat("t".into()),
            allowed_repos: repos.iter().map(|r| normalize_repo(r)).collect(),
            role: GitRole::Principal,
        }
    }

    #[test]
    fn subagent_cannot_merge_principal_can() {
        let principal = GitWriteConfig {
            credential: GitCredential::Pat("t".into()),
            allowed_repos: vec!["a/b".into()],
            role: GitRole::Principal,
        };
        let sub = GitWriteConfig {
            credential: GitCredential::Pat("t".into()),
            allowed_repos: vec!["a/b".into()],
            role: GitRole::SubAgent,
        };
        assert!(principal.can_merge());
        assert!(!sub.can_merge());
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        let c = cfg(&[]);
        assert!(!c.repo_allowed("owner/repo"));
    }

    #[test]
    fn allow_is_case_and_dotgit_insensitive() {
        let c = cfg(&["pallakatos/kars-pr-e2e-demo"]);
        assert!(c.repo_allowed("pallakatos/kars-pr-e2e-demo"));
        assert!(c.repo_allowed("Pallakatos/Kars-PR-E2E-Demo"));
        assert!(c.repo_allowed("pallakatos/kars-pr-e2e-demo.git"));
        assert!(!c.repo_allowed("pallakatos/other-repo"));
        assert!(!c.repo_allowed("someoneelse/kars-pr-e2e-demo"));
    }

    #[test]
    fn parse_repos_filters_junk() {
        let v = parse_repos(" a/b , , c/d.git ,nope, e/f ");
        assert_eq!(v, vec!["a/b", "c/d", "e/f"]);
    }

    #[tokio::test]
    async fn pat_token_is_returned() {
        let c = cfg(&["a/b"]);
        assert_eq!(c.token().await.unwrap(), "t");
    }
}
