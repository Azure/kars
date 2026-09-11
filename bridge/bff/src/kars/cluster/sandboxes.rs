use super::{Cluster, ContainerState, PodHealth};
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use kube::api::{Api, DynamicObject, GroupVersionKind, ListParams};
use kube::core::ApiResource;

pub(super) fn descendant_sandbox_objects(
    sandboxes: &[DynamicObject],
    root: &str,
) -> Vec<DynamicObject> {
    let mut descendants = Vec::new();
    let mut frontier = vec![root.to_string()];
    let mut seen = std::collections::HashSet::from([root.to_string()]);
    while let Some(parent) = frontier.pop() {
        for sandbox in sandboxes {
            let Some(name) = sandbox.metadata.name.as_ref() else {
                continue;
            };
            let is_child = sandbox
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get("kars.azure.com/parent"))
                == Some(&parent);
            if is_child && seen.insert(name.clone()) {
                descendants.push(sandbox.clone());
                frontier.push(name.clone());
            }
        }
    }
    descendants
}

impl Cluster {
    /// Find the name of the **Running** pod for a sandbox in its namespace.
    /// A task-materialized sandbox runs in namespace `kars-<sandbox>`; its pod
    /// carries `kars.azure.com/sandbox=<sandbox>`. Returns `None` if no Running
    /// pod is found.
    /// Whether a Deployment matching `name` exists in `namespace` (best-effort;
    /// false on any API error). Used to detect optional integrations like the
    /// Headlamp dashboard (`headlamp` deployment in the `headlamp` namespace).
    pub async fn deployment_exists(&self, namespace: &str, name: &str) -> bool {
        use k8s_openapi::api::apps::v1::Deployment;
        let api: Api<Deployment> = Api::namespaced(self.client.clone(), namespace);
        matches!(api.get_opt(name).await, Ok(Some(_)))
    }

    /// Every pod in the kars-relevant namespaces (all `kars*` namespaces plus
    /// `agentmesh`), for the operator diagnostics scan. Uses a cluster-wide list
    /// then filters, so it's one API call regardless of sandbox count.
    pub async fn all_pods(&self) -> Vec<k8s_openapi::api::core::v1::Pod> {
        let pods: Api<Pod> = Api::all(self.client.clone());
        pods.list(&ListParams::default())
            .await
            .map(|l| {
                l.items
                    .into_iter()
                    .filter(|p| {
                        let ns = p.metadata.namespace.as_deref().unwrap_or("");
                        ns.starts_with("kars") || ns == "agentmesh"
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn running_pod_for_sandbox(&self, sandbox: &str) -> Option<String> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &ns);
        let list = pods
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={sandbox}")))
            .await
            .ok()?;
        list.items.into_iter().find_map(|p| {
            let phase = p.status.as_ref().and_then(|s| s.phase.as_deref());
            if phase == Some("Running") {
                p.metadata.name
            } else {
                None
            }
        })
    }

    /// Honest health of a sandbox's running pod: container readiness, restart
    /// count, uptime, and node. No metrics-server dependency (no CPU/mem) — these
    /// are status-derived signals that answer "is this agent healthy right now".
    /// `None` when no pod is running for the sandbox.
    pub async fn sandbox_pod_health(&self, sandbox: &str) -> Option<PodHealth> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &ns);
        let list = pods
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={sandbox}")))
            .await
            .ok()?;
        let pod = list
            .items
            .into_iter()
            .find(|p| p.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running"))?;
        let status = pod.status.as_ref();
        let cs = status.and_then(|s| s.container_statuses.as_ref());
        let total = cs.map(|c| c.len()).unwrap_or(0) as i32;
        let ready = cs
            .map(|c| c.iter().filter(|s| s.ready).count())
            .unwrap_or(0) as i32;
        let restarts = cs
            .map(|c| c.iter().map(|s| s.restart_count).sum())
            .unwrap_or(0);
        // Uptime from the pod start time.
        let uptime_seconds = status
            .and_then(|s| s.start_time.as_ref())
            .map(|t| (chrono::Utc::now() - t.0).num_seconds().max(0));
        // A container stuck waiting (e.g. CrashLoopBackOff) is the honest
        // unhealthy signal — surface the reason.
        let waiting_reason = cs.and_then(|c| {
            c.iter().find_map(|s| {
                s.state
                    .as_ref()
                    .and_then(|st| st.waiting.as_ref())
                    .and_then(|w| w.reason.clone())
            })
        });
        Some(PodHealth {
            ready_containers: ready,
            total_containers: total,
            restarts,
            uptime_seconds,
            node: pod.spec.as_ref().and_then(|s| s.node_name.clone()),
            waiting_reason,
        })
    }

    /// Read recent logs from a sandbox pod container (best-effort). Powers the
    /// live run-failure troubleshooter, which surfaces the REAL agent output as
    /// evidence rather than pattern-matching a status string.
    pub async fn read_sandbox_logs(
        &self,
        sandbox: &str,
        container: &str,
        tail: i64,
    ) -> Option<String> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &ns);
        let list = pods
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={sandbox}")))
            .await
            .ok()?;
        let pod_name = list.items.into_iter().find_map(|p| p.metadata.name)?;
        let lp = kube::api::LogParams {
            container: Some(container.to_string()),
            tail_lines: Some(tail),
            timestamps: false,
            ..Default::default()
        };
        pods.logs(&pod_name, &lp).await.ok()
    }

    /// Per-container status for a sandbox pod (name, ready, restarts, and the
    /// current state reason — Running / a waiting reason like ImagePullBackOff /
    /// a terminated reason like OOMKilled). Used by the troubleshooter.
    pub async fn sandbox_container_states(&self, sandbox: &str) -> Vec<ContainerState> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &ns);
        let Ok(list) = pods
            .list(&ListParams::default().labels(&format!("kars.azure.com/sandbox={sandbox}")))
            .await
        else {
            return Vec::new();
        };
        let Some(pod) = list.items.into_iter().next() else {
            return Vec::new();
        };
        let cs = pod
            .status
            .as_ref()
            .and_then(|s| s.container_statuses.as_ref());
        cs.map(|list| {
            list.iter()
                .map(|c| {
                    let (state, reason) = if let Some(st) = c.state.as_ref() {
                        if st.running.is_some() {
                            ("running".to_string(), None)
                        } else if let Some(w) = st.waiting.as_ref() {
                            ("waiting".to_string(), w.reason.clone())
                        } else if let Some(t) = st.terminated.as_ref() {
                            ("terminated".to_string(), t.reason.clone())
                        } else {
                            ("unknown".to_string(), None)
                        }
                    } else {
                        ("unknown".to_string(), None)
                    };
                    ContainerState {
                        name: c.name.clone(),
                        ready: c.ready,
                        restarts: c.restart_count,
                        state,
                        reason,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// The live egress enforcement mode of a sandbox — read from the
    /// `KarsSandbox.spec.networkPolicy.egressMode` the controller materialized.
    /// `"Learn"` (default) observes + records every domain the agent reaches
    /// without denying; `"Strict"` denies anything outside the allowlist. This
    /// is the real, cluster-truth mode — not derived from the blueprint.
    pub async fn sandbox_egress_mode(&self, sandbox: &str) -> Option<String> {
        let gvk = GroupVersionKind::gvk("kars.azure.com", "v1alpha1", "KarsSandbox");
        let ar = ApiResource::from_gvk(&gvk);
        let api: Api<DynamicObject> = Api::namespaced_with(self.client.clone(), "kars-system", &ar);
        let sb = api.get_opt(sandbox).await.ok().flatten()?;
        Some(
            sb.data
                .get("spec")
                .and_then(|s| s.get("networkPolicy"))
                .and_then(|n| n.get("egressMode"))
                .and_then(|m| m.as_str())
                .unwrap_or("Learn")
                .to_string(),
        )
    }

    /// Read only the declared private observation capability. The legacy
    /// agent-shared admin token and apiserver header tricks are never fallbacks.
    pub async fn sandbox_learned_domains(&self, sandbox: &str) -> anyhow::Result<Vec<String>> {
        self.private_learned_domains(sandbox)
            .await
            .map_err(Into::into)
    }

    /// The resolved egress allowlist the sandbox actually enforces — read from
    /// the `karssandbox-<sandbox>-egress-allowlist` ConfigMap the controller
    /// compiles into the sandbox namespace. Each entry is the exact host(:port)
    /// the agent is permitted to reach. Empty in Learn mode (nothing pinned).
    pub async fn sandbox_allowlist(&self, sandbox: &str) -> Vec<String> {
        let ns = format!("kars-{sandbox}");
        let name = format!("karssandbox-{sandbox}-egress-allowlist");
        let cms: Api<ConfigMap> = Api::namespaced(self.client.clone(), &ns);
        let Some(cm) = cms.get_opt(&name).await.ok().flatten() else {
            return Vec::new();
        };
        let Some(raw) = cm.data.and_then(|d| d.get("allowlist.json").cloned()) else {
            return Vec::new();
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Vec::new();
        };
        parsed
            .get("endpoints")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        let host = e.get("host").and_then(|h| h.as_str())?;
                        match e.get("port").and_then(|p| p.as_u64()) {
                            Some(p) => Some(format!("{host}:{p}")),
                            None => Some(host.to_string()),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// LIVE per-agent execution trace, straight from a running sandbox's router
    /// (`GET /telemetry/trace` — a PUBLIC in-pod endpoint, reached via the
    /// apiserver pod-proxy; no admin token required). Unlike the persisted
    /// `kars-mission-trace-<task>` ConfigMap (written once, at delivery), this
    /// ticks WHILE the agent works, so the activity stream is genuinely live.
    /// Returns the router's `events` array (round/tool shape); empty on any
    /// error or when the sandbox has no running pod yet.
    pub async fn sandbox_live_trace(&self, sandbox: &str) -> Vec<serde_json::Value> {
        let ns = format!("kars-{sandbox}");
        let pods: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(self.client.clone(), &ns);
        let pod_list = pods.list(&ListParams::default()).await;
        if let Err(e) = &pod_list {
            tracing::warn!(target: "kars_bridge::live_trace", %ns, error = %e, "pod list failed");
        }
        let Some(pod) = pod_list
            .ok()
            .and_then(|l| {
                l.items.into_iter().find(|p| {
                    p.status
                        .as_ref()
                        .and_then(|s| s.phase.as_deref())
                        .map(|ph| ph == "Running")
                        .unwrap_or(false)
                })
            })
            .and_then(|p| p.metadata.name)
        else {
            tracing::warn!(target: "kars_bridge::live_trace", %ns, "no running pod found");
            return Vec::new();
        };
        let path = format!("/api/v1/namespaces/{ns}/pods/{pod}:8443/proxy/telemetry/trace");
        let Ok(req) = http::Request::builder()
            .method(http::Method::GET)
            .uri(&path)
            .body(Vec::new())
        else {
            tracing::warn!(target: "kars_bridge::live_trace", %path, "request build failed");
            return Vec::new();
        };
        match self.client.request_text(req).await {
            Ok(text) => {
                let n = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("events").and_then(|e| e.as_array()).cloned())
                    .unwrap_or_default();
                tracing::debug!(target: "kars_bridge::live_trace", %pod, events = n.len(), body_len = text.len(), "live trace ok");
                n
            }
            Err(e) => {
                tracing::warn!(target: "kars_bridge::live_trace", %path, error = %e, "proxy request failed");
                Vec::new()
            }
        }
    }

    /// Names of the sub-agent sandboxes a principal spawned at run time — the
    /// complete transitive `kars.azure.com/parent` tree. Used to aggregate the
    /// WHOLE agent tree's live activity, not just direct children.
    pub async fn sub_agent_sandboxes(
        &self,
        namespace: &str,
        parent_sandbox: &str,
    ) -> Vec<DynamicObject> {
        self.list_kind(namespace, "KarsSandbox")
            .await
            .map(|items| descendant_sandbox_objects(&items, parent_sandbox))
            .unwrap_or_default()
    }

    pub async fn sub_agent_sandbox_names(
        &self,
        namespace: &str,
        parent_sandbox: &str,
    ) -> Vec<String> {
        self.sub_agent_sandboxes(namespace, parent_sandbox)
            .await
            .iter()
            .filter_map(|sandbox| sandbox.metadata.name.clone())
            .collect()
    }
}
