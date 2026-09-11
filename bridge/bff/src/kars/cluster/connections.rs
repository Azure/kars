use super::Cluster;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::ResourceExt;
use kube::api::Api;

impl Cluster {
    // ── Keyless git write: shared App + per-principal connections (§14) ──────

    /// The cluster-shared kars GitHub App credentials (App id + PEM private key)
    /// from `Secret kars-github-app` in kars-system. `None` when the operator
    /// hasn't configured the App — git write is simply off (fail-closed).
    pub async fn github_app_creds(&self) -> Result<Option<(String, String)>, kube::Error> {
        let (_, s) = self
            .integration_store(&self.core_namespace(), "kars-github-app")
            .await?;
        let Some(data) = s.data else { return Ok(None) };
        let read = |k: &str| -> Option<String> {
            data.get(k)
                .and_then(|v| String::from_utf8(v.0.clone()).ok())
        };
        let Some(id) = read("GITHUB_APP_ID") else {
            return Ok(None);
        };
        let Some(key) = read("GITHUB_APP_PRIVATE_KEY") else {
            return Ok(None);
        };
        if id.trim().is_empty() || key.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some((id.trim().to_string(), key)))
    }

    /// Read a principal's GitHub connection ConfigMap in the namespace.
    pub async fn read_github_connection(
        &self,
        ns: &str,
        connection_name: &str,
    ) -> Option<(String, String, Vec<String>)> {
        self.read_github_connection_result(ns, connection_name)
            .await
            .ok()
            .flatten()
    }

    /// Read a principal GitHub connection while preserving Kubernetes API
    /// failures for background jobs that must report an honest source status.
    pub async fn read_github_connection_result(
        &self,
        ns: &str,
        connection_name: &str,
    ) -> Result<Option<(String, String, Vec<String>)>, kube::Error> {
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        let Some(data) = api.get_opt(connection_name).await?.and_then(|cm| cm.data) else {
            return Ok(None);
        };
        let read = |key: &str| data.get(key).cloned();
        let Some(installation_id) = read("installation_id") else {
            return Ok(None);
        };
        let account = read("account").unwrap_or_default();
        let repos = read("repos")
            .and_then(|r| serde_json::from_str::<Vec<String>>(&r).ok())
            .unwrap_or_default();
        Ok(Some((installation_id, account, repos)))
    }

    /// Store a principal's GitHub connection. No token or credential is stored.
    pub async fn write_github_connection(
        &self,
        ns: &str,
        connection_name: &str,
        installation_id: &str,
        account: &str,
        repos: &[String],
    ) -> anyhow::Result<()> {
        use kube::api::{Patch, PatchParams, PostParams};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        let data = std::collections::BTreeMap::from([
            ("installation_id".to_string(), installation_id.to_string()),
            ("account".to_string(), account.to_string()),
            ("repos".to_string(), serde_json::to_string(repos)?),
        ]);
        let patch = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": connection_name,
                "namespace": ns,
                "labels": { "app.kubernetes.io/managed-by": "kars-bridge", "kars.azure.com/github-connection": "true" },
            },
            "data": data,
        });
        if let Some(current) = api.get_opt(connection_name).await? {
            let grant = self.credential_grant(ns).await?;
            if current.metadata.deletion_timestamp.is_some()
                || current.uid().is_none()
                || !grant.document.data["spec"]["githubConnections"]
                    .as_array()
                    .is_some_and(|entries| {
                        entries.iter().any(|entry| {
                            entry["connection"]["name"] == connection_name
                                && entry["connection"]["uid"]
                                    == serde_json::json!(current.metadata.uid)
                        })
                    })
            {
                anyhow::bail!(
                    "Existing GitHub connection requires exact operator UID enrollment before mutation; no adoption"
                );
            }
            api.patch(connection_name,&PatchParams::default(),&Patch::Merge(serde_json::json!({
                "metadata":{"uid":current.metadata.uid,"resourceVersion":current.metadata.resource_version},"data":data
            }))).await?;
        } else {
            let created: ConfigMap = serde_json::from_value(patch)?;
            api.create(&PostParams::default(), &created).await?;
        }
        Ok(())
    }

    /// Remove only the named principal GitHub connection.
    pub async fn delete_github_connection(
        &self,
        ns: &str,
        connection_name: &str,
    ) -> anyhow::Result<()> {
        use kube::api::{DeleteParams, Preconditions};
        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), ns);
        if let Some(current) = api.get_opt(connection_name).await? {
            if current.uid().is_none() || current.resource_version().is_none() {
                anyhow::bail!("GitHub connection identity is unavailable; no deletion");
            }
            api.delete(
                connection_name,
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: current.metadata.uid,
                        resource_version: current.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await?;
        }
        Ok(())
    }

    /// Which communication-channel env keys a team has configured. SECURITY:
    /// returns only the *key names* (e.g. `TELEGRAM_BOT_TOKEN`), never the token
    /// values — the Bridge must never echo a secret back to a browser.
    pub async fn team_channel_keys(
        &self,
        namespace: &str,
        team: &str,
    ) -> Result<Vec<String>, kube::Error> {
        let target = self
            .credential_target(namespace, "KarsTeam", team)
            .await?
            .ok_or_else(|| super::credentials::failure("Team credential target does not exist"))?;
        self.configured_channel_keys(namespace, Some(&target)).await
    }

    /// Merge channel credentials into a team's channel Secret (create if absent).
    /// SECURITY: token values are written straight into a K8s Secret and are
    /// never logged or returned. Existing keys not in `data` are preserved.
    pub async fn merge_team_channel(
        &self,
        namespace: &str,
        team: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        let target = self
            .credential_target(namespace, "KarsTeam", team)
            .await?
            .ok_or_else(|| super::credentials::failure("Team credential target does not exist"))?;
        self.write_agent_credentials(
            namespace,
            "KarsTeam",
            team,
            Some(&target.uid),
            data,
            Vec::new(),
        )
        .await?;
        Ok(())
    }

    /// Remove specific channel env keys from a team's channel Secret; delete the
    /// Secret entirely when no keys remain (so "disable all channels" is clean).
    pub async fn remove_team_channel_keys(
        &self,
        namespace: &str,
        team: &str,
        keys: &[String],
    ) -> anyhow::Result<()> {
        let target = self
            .credential_target(namespace, "KarsTeam", team)
            .await?
            .ok_or_else(|| super::credentials::failure("Team credential target does not exist"))?;
        self.write_agent_credentials(
            namespace,
            "KarsTeam",
            team,
            Some(&target.uid),
            std::collections::BTreeMap::new(),
            keys.to_vec(),
        )
        .await?;
        Ok(())
    }

    // ─── Workspace-level (agent-agnostic) channels ───────────────────────────
    // The same channel model as a team's, but scoped to the WORKSPACE (secret
    // `kars-workspace-channels` in kars-system), configured on the Connections
    // tab. The controller propagates it into EVERY run sandbox — mission or team —
    // so any agent can report over Telegram/Slack/Discord/WhatsApp.

    /// The env-key names present in the workspace channel Secret (no values).
    pub async fn workspace_channel_keys(
        &self,
        namespace: &str,
    ) -> Result<Vec<String>, kube::Error> {
        let mut keys = self.configured_channel_keys(namespace, None).await?;
        if self.teams_configured().await? {
            keys.push("TEAMS_ENABLED".into());
        }
        Ok(keys)
    }

    /// Merge channel credentials into the workspace channel Secret (create if
    /// absent). Token values are written straight into a K8s Secret, never logged
    /// or returned. Existing keys not in `data` are preserved.
    pub async fn merge_workspace_channel(
        &self,
        namespace: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        self.write_agent_credentials(namespace, "Workspace", namespace, None, data, Vec::new())
            .await?;
        Ok(())
    }

    /// Remove specific channel env keys from the workspace channel Secret; delete
    /// the Secret entirely when no keys remain.
    pub async fn remove_workspace_channel_keys(
        &self,
        namespace: &str,
        keys: &[String],
    ) -> anyhow::Result<()> {
        self.write_agent_credentials(
            namespace,
            "Workspace",
            namespace,
            None,
            std::collections::BTreeMap::new(),
            keys.to_vec(),
        )
        .await?;
        Ok(())
    }

    /// Write Teams gateway credentials into the dedicated `kars-bridge-teams` Secret.
    /// This Secret is mounted ONLY by the Teams gateway pod — never propagated to
    /// sandbox pods. Uses Server-Side Apply so the BFF can create-or-update idempotently.
    pub async fn write_dedicated_teams_secret(
        &self,
        namespace: &str,
        name: &str,
        data: std::collections::BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        self.mutate_integration(namespace, name, |keys| keys.extend(data.clone()))
            .await?;
        Ok(())
    }

    /// Revoke Teams bot credentials while retaining the BFF-only internal
    /// secret and role map required for a healthy BFF rollout.
    pub async fn disable_dedicated_teams_secret(
        &self,
        namespace: &str,
        name: &str,
    ) -> anyhow::Result<()> {
        self.mutate_integration(namespace, name, |keys| {
            for key in ["client-id", "tenant-id", "client-secret"] {
                keys.remove(key);
            }
        })
        .await?;
        Ok(())
    }

    /// Restart BFF and enable/disable the Teams gateway so Secret and role-map
    /// changes become effective immediately.
    pub async fn reconcile_teams_deployments(
        &self,
        namespace: &str,
        gateway_name: &str,
        bff_name: &str,
        _enabled: bool,
    ) -> anyhow::Result<()> {
        self.request_teams_reconcile(namespace, gateway_name, bff_name)
            .await?;
        Ok(())
    }
    /// Upsert a Secret via server-side apply, merging keys without clobbering
    /// existing ones. Used to store agent credentials (write-only); the value is
    /// never read back through any endpoint.
    pub async fn upsert_secret(
        &self,
        namespace: &str,
        name: &str,
        body: serde_json::Value,
    ) -> Result<(), kube::Error> {
        let data = body
            .get("stringData")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| super::credentials::failure("Integration update requires stringData"))?;
        let values = data
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_string()))
                    .ok_or_else(|| {
                        super::credentials::failure("Integration value must be a string")
                    })
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
        self.mutate_integration(namespace, name, |keys| keys.extend(values.clone()))
            .await
    }

    /// Delete a write-only credential Secret this Bridge authored (e.g. the
    /// shared `kars-github-app` Secret on disconnect). A 404 is not an error —
    /// the secret is already absent, which is the caller's desired end state.
    pub async fn delete_secret(&self, namespace: &str, name: &str) -> Result<(), kube::Error> {
        self.mutate_integration(namespace, name, |keys| keys.clear())
            .await
    }

    /// Read a single key's value from a Secret (base64-decoded UTF-8). `None`
    /// when the secret/key is absent. Used by the Foundry preflight to make a
    /// real authenticated call with the onboarded key — the key never leaves the
    /// BFF process.
    pub async fn read_secret_value(
        &self,
        namespace: &str,
        secret: &str,
        key: &str,
    ) -> Result<Option<String>, kube::Error> {
        let (_, s) = self.integration_store(namespace, secret).await?;
        if let Some(v) = s.data.as_ref().and_then(|d| d.get(key)) {
            return String::from_utf8(v.0.clone())
                .map(Some)
                .map_err(|_| super::credentials::failure("Credential value is not UTF-8"));
        }
        Ok(None)
    }

    /// Read every key of a Secret as UTF-8 strings (base64-decoded). Empty map
    /// when the secret doesn't exist. Used for the multi-provider inference
    /// Secret, whose keys ARE the literal env var names the router reads
    /// (`KARS_PROVIDER_<TAG>_ENDPOINT`, `COPILOT_GITHUB_TOKEN`, ...) — listing
    /// requires reading the whole key set, not one key at a time.
    pub async fn read_secret_all(
        &self,
        namespace: &str,
        secret: &str,
    ) -> Result<std::collections::BTreeMap<String, String>, kube::Error> {
        let (_, s) = self.integration_store(namespace, secret).await?;
        Ok(Self::decode_secret_data(&s))
    }

    /// Read-modify-write a Secret's full key set under real optimistic
    /// concurrency (CAS): a single atomic JSON Patch (RFC 6902) — a `test` op
    /// asserting `resourceVersion` hasn't moved, followed by `add`/`remove`
    /// ops for the actual key changes — retried on failure.
    ///
    /// Why JSON Patch specifically, not `replace()`/PUT or a plain JSON merge
    /// patch:
    ///   - `replace()` (PUT) is the "update" RBAC verb, which the BFF's
    ///     ClusterRole deliberately never grants (write access here is
    ///     `create`/`patch` only) — using it would 403 in any real
    ///     RBAC-enforced deployment. Confirmed live against the actual
    ///     ServiceAccount (not a developer's cluster-admin kubeconfig).
    ///   - A plain JSON *merge* patch (RFC 7396, what this function used
    ///     before) uses the `patch` verb correctly, but the K8s API does NOT
    ///     honor `resourceVersion` as a precondition for merge patches —
    ///     confirmed live: a merge patch carrying a stale resourceVersion
    ///     still applies. So a merge patch alone has no way to detect a
    ///     concurrent writer.
    ///   - JSON Patch's `test` op DOES enforce the precondition atomically
    ///     alongside the real mutation (confirmed live: a stale
    ///     resourceVersion in a `test` op → the whole patch is rejected,
    ///     HTTP 422, and none of the following ops apply) — and it's still
    ///     the `patch` verb, so no RBAC widening is needed.
    ///   - Field removal still works here (unlike Server-Side-Apply, whose
    ///     merge semantics never remove an absent key) via an explicit
    ///     `remove` op per dropped key.
    pub async fn mutate_secret_keys(
        &self,
        namespace: &str,
        secret: &str,
        mutate: impl Fn(&mut std::collections::BTreeMap<String, String>),
    ) -> Result<(), kube::Error> {
        self.mutate_integration(namespace, secret, mutate).await
    }

    /// Decode a Secret's `data` (+ any pending `stringData`) into a flat map,
    /// the shared helper behind both `read_secret_all` and the CAS loop above.
    fn decode_secret_data(
        s: &k8s_openapi::api::core::v1::Secret,
    ) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        if let Some(d) = s.data.as_ref() {
            for (k, v) in d {
                if let Ok(s) = String::from_utf8(v.0.clone()) {
                    out.insert(k.clone(), s);
                }
            }
        }
        if let Some(d) = s.string_data.as_ref() {
            for (k, v) in d {
                out.insert(k.clone(), v.clone());
            }
        }
        out
    }
}
