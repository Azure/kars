// kars Bridge BFF — shared application state.
//
// Holds the optional cluster handle. Cluster connectivity is *optional* at
// startup: the BFF serves health immediately and reports cluster wiring
// honestly via readiness, rather than crash-looping when no cluster is
// reachable (e.g. local web-only development).

use std::sync::Arc;

use crate::kars::cluster::Cluster;

/// Axum shared state, cheaply cloneable.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    cluster: Option<Cluster>,
    default_namespace: String,
    api_token: Option<String>,
    principal_secret: Option<String>,
    teams_internal_secret: Option<String>,
    /// Entra subject → Bridge roles mapping loaded from BRIDGE_TEAMS_ENTRA_ROLE_MAP.
    /// The BFF resolves principals from this map; it never trusts roles from the
    /// gateway request body.
    teams_entra_role_map: Vec<(String, String, Vec<String>, String)>,
}

impl AppState {
    #[cfg(test)]
    pub(crate) fn for_test_client(client: kube::Client, namespace: &str) -> Self {
        let mut state = Self::web_only(namespace.to_string());
        Arc::get_mut(&mut state.inner).unwrap().cluster = Some(Cluster::for_test_client(client));
        state
    }

    /// Build state, attempting a cluster connection. A failed connection is
    /// not fatal — `cluster()` returns `None` and readiness reports it.
    pub async fn new(default_namespace: String) -> Self {
        let cluster = match Cluster::connect().await {
            Ok(c) => {
                tracing::info!("connected to cluster");
                Some(c)
            }
            Err(e) => {
                tracing::warn!(error = %e, "no cluster connection — running web-only");
                None
            }
        };
        Self {
            inner: Arc::new(Inner {
                cluster,
                default_namespace,
                api_token: None,
                principal_secret: None,
                teams_internal_secret: None,
                teams_entra_role_map: Vec::new(),
            }),
        }
    }

    /// Attach the mutating-endpoint bearer token (from BRIDGE_API_TOKEN).
    #[must_use]
    pub fn with_api_token(self, token: Option<String>) -> Self {
        let inner = self.inner;
        Self {
            inner: Arc::new(Inner {
                cluster: inner.cluster.clone(),
                default_namespace: inner.default_namespace.clone(),
                api_token: token,
                principal_secret: inner.principal_secret.clone(),
                teams_internal_secret: inner.teams_internal_secret.clone(),
                teams_entra_role_map: inner.teams_entra_role_map.clone(),
            }),
        }
    }

    #[must_use]
    pub fn with_principal_secret(self, secret: Option<String>) -> Self {
        let inner = self.inner;
        Self {
            inner: Arc::new(Inner {
                cluster: inner.cluster.clone(),
                default_namespace: inner.default_namespace.clone(),
                api_token: inner.api_token.clone(),
                principal_secret: secret,
                teams_internal_secret: inner.teams_internal_secret.clone(),
                teams_entra_role_map: inner.teams_entra_role_map.clone(),
            }),
        }
    }

    /// Attach the Teams gateway internal shared secret (from BRIDGE_TEAMS_INTERNAL_SECRET).
    #[must_use]
    pub fn with_teams_internal_secret(self, secret: Option<String>) -> Self {
        let inner = self.inner;
        Self {
            inner: Arc::new(Inner {
                cluster: inner.cluster.clone(),
                default_namespace: inner.default_namespace.clone(),
                api_token: inner.api_token.clone(),
                principal_secret: inner.principal_secret.clone(),
                teams_internal_secret: secret,
                teams_entra_role_map: inner.teams_entra_role_map.clone(),
            }),
        }
    }

    /// Attach the Entra→Bridge role map for Teams principal resolution.
    /// Parsed from BRIDGE_TEAMS_ENTRA_ROLE_MAP JSON.
    #[must_use]
    pub fn with_teams_entra_role_map(
        self,
        map: Vec<(String, String, Vec<String>, String)>,
    ) -> Self {
        let inner = self.inner;
        Self {
            inner: Arc::new(Inner {
                cluster: inner.cluster.clone(),
                default_namespace: inner.default_namespace.clone(),
                api_token: inner.api_token.clone(),
                principal_secret: inner.principal_secret.clone(),
                teams_internal_secret: inner.teams_internal_secret.clone(),
                teams_entra_role_map: map,
            }),
        }
    }

    /// The bearer token guarding mutating endpoints, if configured.
    pub fn api_token(&self) -> Option<&str> {
        self.inner.api_token.as_deref()
    }

    pub fn principal_secret(&self) -> Option<&str> {
        self.inner.principal_secret.as_deref()
    }

    /// The shared secret for the Teams gateway internal decision endpoint.
    pub fn teams_internal_secret(&self) -> Option<&str> {
        self.inner.teams_internal_secret.as_deref()
    }

    /// The Entra→Bridge role map for Teams principal resolution.
    /// Each entry is (entra_subject, bridge_roles, display_name).
    pub fn teams_entra_role_map(&self) -> &[(String, String, Vec<String>, String)] {
        &self.inner.teams_entra_role_map
    }

    /// The cluster handle, if one was established.
    pub fn cluster(&self) -> Option<&Cluster> {
        self.inner.cluster.as_ref()
    }

    /// The namespace used for readiness probing + default scoping.
    pub fn default_namespace(&self) -> &str {
        &self.inner.default_namespace
    }

    /// Build state with no cluster connection — for tests and explicit
    /// web-only mode. Deterministic regardless of ambient kubeconfig.
    pub fn web_only(default_namespace: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                cluster: None,
                default_namespace,
                api_token: None,
                principal_secret: None,
                teams_internal_secret: None,
                teams_entra_role_map: Vec::new(),
            }),
        }
    }
}
