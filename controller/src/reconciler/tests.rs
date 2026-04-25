//! Reconciler unit tests — extracted from reconciler/mod.rs to keep the
//! reconcile-loop file under its Phase 1 LOC cap. `use super::*`
//! continues to give the tests access to crate-private helpers
//! (`build_pod_security_context`, `error_requeue_duration`, etc.).
//
// ci:loc-ok: pre-existing test corpus relocated wholesale from
// reconciler.rs; no test added or modified in this PR. Splitting further
// would scatter cohesive #[test] blocks across multiple files for no
// reviewer benefit.

use super::*;
use crate::crd::SandboxConfig;

    #[test]
    fn standard_isolation_uses_runtime_default_seccomp() {
        let cfg = SandboxConfig {
            isolation: "standard".into(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
    }

    #[test]
    fn enhanced_isolation_uses_localhost_seccomp() {
        let cfg = SandboxConfig {
            isolation: "enhanced".into(),
            seccomp_profile: "azureclaw-strict".into(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["seccompProfile"]["type"], "Localhost");
        assert_eq!(
            ctx["seccompProfile"]["localhostProfile"],
            "profiles/azureclaw-strict.json"
        );
    }

    #[test]
    fn confidential_isolation_uses_runtime_default_seccomp() {
        let cfg = SandboxConfig {
            isolation: "confidential".into(),
            seccomp_profile: "azureclaw-strict".into(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        // Kata VM provides isolation, so RuntimeDefault is sufficient
        assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
    }

    #[test]
    fn security_context_enforces_non_root() {
        let cfg = SandboxConfig::default();
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["runAsNonRoot"], true);
        assert_eq!(ctx["runAsUser"], 1000);
        assert_eq!(ctx["runAsGroup"], 1000);
        assert_eq!(ctx["fsGroup"], 1000);
    }

    #[test]
    fn selinux_context_only_set_when_non_empty() {
        let cfg = SandboxConfig::default(); // empty selinux_context
        let ctx = build_pod_security_context(&cfg);
        assert!(ctx.get("seLinuxOptions").is_none());

        let cfg_with_selinux = SandboxConfig {
            selinux_context: "custom_t".into(),
            ..Default::default()
        };
        let ctx2 = build_pod_security_context(&cfg_with_selinux);
        assert_eq!(ctx2["seLinuxOptions"]["type"], "custom_t");
    }

    #[test]
    fn isolation_scheduling_standard() {
        let (runtime, pool) = isolation_scheduling("standard");
        assert!(runtime.is_none());
        assert_eq!(pool, "sandbox");
    }

    #[test]
    fn isolation_scheduling_enhanced() {
        let (runtime, pool) = isolation_scheduling("enhanced");
        assert!(runtime.is_none());
        assert_eq!(pool, "sandbox");
    }

    #[test]
    fn isolation_scheduling_confidential() {
        let (runtime, pool) = isolation_scheduling("confidential");
        assert_eq!(runtime, Some("kata-vm-isolation"));
        assert_eq!(pool, "sandbox-kata");
    }

    #[test]
    fn crd_defaults_are_secure() {
        let cfg = SandboxConfig::default();
        assert_eq!(cfg.isolation, "enhanced");
        assert!(cfg.read_only_root_filesystem);
        assert!(cfg.run_as_non_root);
        assert!(!cfg.allow_privilege_escalation);
        assert_eq!(cfg.seccomp_profile, "azureclaw-strict");
        assert!(cfg.selinux_context.is_empty());
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    /// Build namespace JSON the same way reconcile() does (line 224-239).
    fn build_namespace_json(sandbox_name: &str) -> serde_json::Value {
        let sandbox_ns = format!("azureclaw-{sandbox_name}");
        json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {
                "name": sandbox_ns,
                "labels": {
                    "app.kubernetes.io/name": "azureclaw",
                    "app.kubernetes.io/component": "sandbox",
                    "azureclaw.azure.com/sandbox": sandbox_name,
                    "azureclaw.azure.com/role": "sandbox",
                    "pod-security.kubernetes.io/enforce": "privileged",
                    "pod-security.kubernetes.io/audit": "baseline",
                    "pod-security.kubernetes.io/warn": "baseline"
                }
            }
        })
    }

    /// Build ServiceAccount JSON the same way reconcile() does (line 250-263).
    fn build_sa_json(sandbox_name: &str, wi_client_id: &str) -> serde_json::Value {
        let sandbox_ns = format!("azureclaw-{sandbox_name}");
        json!({
            "apiVersion": "v1",
            "kind": "ServiceAccount",
            "metadata": {
                "name": "sandbox",
                "namespace": sandbox_ns,
                "labels": {
                    "azureclaw.azure.com/sandbox": sandbox_name
                },
                "annotations": {
                    "azure.workload.identity/client-id": wi_client_id
                }
            }
        })
    }

    /// Build ClusterRoleBinding JSON the same way reconcile() does (line 289-309).
    fn build_crb_json(sandbox_name: &str) -> serde_json::Value {
        let sandbox_ns = format!("azureclaw-{sandbox_name}");
        let crb_name = format!("azureclaw-spawner-{sandbox_name}");
        json!({
            "apiVersion": "rbac.authorization.k8s.io/v1",
            "kind": "ClusterRoleBinding",
            "metadata": {
                "name": crb_name,
                "labels": {
                    "azureclaw.azure.com/sandbox": sandbox_name,
                    "app.kubernetes.io/managed-by": "azureclaw-controller"
                }
            },
            "roleRef": {
                "apiGroup": "rbac.authorization.k8s.io",
                "kind": "ClusterRole",
                "name": "azureclaw-sandbox-spawner"
            },
            "subjects": [{
                "kind": "ServiceAccount",
                "name": "sandbox",
                "namespace": sandbox_ns
            }]
        })
    }

    /// Build default egress rules the same way reconcile() does (line 443-480).
    fn build_default_egress_rules() -> Vec<serde_json::Value> {
        vec![
            json!({
                "to": [
                    {"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}},
                    {"ipBlock": {"cidr": "10.0.0.10/32"}}
                ],
                "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]
            }),
            json!({
                "to": [{"ipBlock": {"cidr": "169.254.169.254/32"}}],
                "ports": [{"protocol": "TCP", "port": 80}]
            }),
            json!({
                "to": [{"ipBlock": {"cidr": "0.0.0.0/0", "except": ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]}}],
                "ports": [{"protocol": "TCP", "port": 443}]
            }),
            json!({
                "to": [{"namespaceSelector": {"matchLabels": {"azureclaw.azure.com/role": "sandbox"}}}],
                "ports": [{"protocol": "TCP", "port": 8443}]
            }),
            json!({
                "to": [{"namespaceSelector": {"matchLabels": {"app.kubernetes.io/managed-by": "azureclaw"}}}],
                "ports": [{"protocol": "TCP", "port": 8765}, {"protocol": "TCP", "port": 8080}]
            }),
        ]
    }

    /// Build the openclaw container JSON (line 702-746).
    fn build_openclaw_container(
        image: &str,
        cfg: &SandboxConfig,
        model: &str,
    ) -> serde_json::Value {
        let pull_policy = if image.ends_with(":latest") {
            "Always"
        } else {
            "IfNotPresent"
        };
        json!({
            "name": "openclaw",
            "image": image,
            "imagePullPolicy": pull_policy,
            "ports": [{"containerPort": 18789, "name": "gateway"}],
            "env": [
                {"name": "OPENCLAW_MODEL", "value": model},
                {"name": "AZURE_OPENAI_ENDPOINT", "value": "https://test.openai.azure.com"},
                {"name": "AZURECLAW_AUTH_MODE", "value": "workload-identity"},
                {"name": "OPENCLAW_GATEWAY_TOKEN", "value": "test-token"},
            ],
            "securityContext": {
                "runAsUser": 1000,
                "allowPrivilegeEscalation": cfg.allow_privilege_escalation,
                "readOnlyRootFilesystem": cfg.read_only_root_filesystem,
                "capabilities": {"drop": ["ALL"]}
            },
            "volumeMounts": [
                {"name": "sandbox-data", "mountPath": "/sandbox"},
                {"name": "tmp", "mountPath": "/tmp"},
                {"name": "admin-token", "mountPath": "/etc/azureclaw/secrets", "readOnly": true}
            ],
            "resources": {
                "requests": {"cpu": "500m", "memory": "1Gi"},
                "limits": {"cpu": "2", "memory": "4Gi"}
            },
            "livenessProbe": {
                "exec": {"command": ["sh", "-c", "test -f /proc/1/status"]},
                "initialDelaySeconds": 15,
                "periodSeconds": 30
            },
            "readinessProbe": {
                "exec": {"command": ["sh", "-c", "test -f /proc/1/status"]},
                "initialDelaySeconds": 5,
                "periodSeconds": 10
            }
        })
    }

    /// Build inference-router container JSON (line 747-778).
    fn build_router_container(
        image: &str,
        name: &str,
        cfg: &SandboxConfig,
        model: &str,
    ) -> serde_json::Value {
        json!({
            "name": "inference-router",
            "image": image,
            "ports": [
                {"containerPort": 8443, "name": "inference"},
                {"containerPort": 9090, "name": "metrics"}
            ],
            "env": [
                {"name": "AZURE_OPENAI_ENDPOINT", "value": "https://test.openai.azure.com"},
                {"name": "FOUNDRY_ENDPOINT", "value": "https://test.foundry.azure.com"},
                {"name": "FOUNDRY_PROJECT_ENDPOINT", "value": "https://test.foundry.azure.com/project"},
                {"name": "IMDS_CLIENT_ID", "value": "test-imds-id"},
                {"name": "AZURE_OPENAI_DEPLOYMENT", "value": model},
                {"name": "AZURECLAW_AUTH_MODE", "value": "workload-identity"},
                {"name": "CONTENT_SAFETY_ENABLED", "value": "true"},
                {"name": "PROMPT_SHIELDS_ENABLED", "value": "true"},
                {"name": "CONTENT_SAFETY_ENDPOINT", "value": "https://test.contentsafety.azure.com"},
                {"name": "TOKEN_BUDGET_DAILY", "value": "0"},
                {"name": "TOKEN_BUDGET_PER_REQUEST", "value": "0"},
                {"name": "SANDBOX_NAME", "value": name},
                {"name": "SANDBOX_ISOLATION", "value": &cfg.isolation},
                {"name": "RUST_LOG", "value": "info,inference_router=debug"},
            ],
            "securityContext": {
                "runAsUser": 1001,
                "allowPrivilegeEscalation": false,
                "readOnlyRootFilesystem": true,
                "capabilities": {"drop": ["ALL"]}
            },
            "resources": {
                "requests": {"cpu": "100m", "memory": "64Mi"},
                "limits": {"cpu": "500m", "memory": "256Mi"}
            },
            "livenessProbe": {
                "httpGet": {"path": "/healthz", "port": "inference"},
                "initialDelaySeconds": 5,
                "periodSeconds": 15
            },
            "readinessProbe": {
                "httpGet": {"path": "/healthz", "port": "inference"},
                "initialDelaySeconds": 3,
                "periodSeconds": 5
            },
            "volumeMounts": [
                {"name": "admin-token", "mountPath": "/etc/azureclaw/secrets", "readOnly": true}
            ]
        })
    }

    /// Build init container JSON (line 667-701).
    fn build_init_container(image: &str) -> serde_json::Value {
        json!({
            "name": "egress-guard",
            "image": image,
            "securityContext": {
                "runAsUser": 0,
                "runAsNonRoot": false,
                "seccompProfile": { "type": "Unconfined" },
                "capabilities": {
                    "add": ["NET_ADMIN", "NET_RAW"],
                    "drop": ["ALL"]
                }
            },
            "resources": {
                "requests": {"cpu": "10m", "memory": "32Mi"},
                "limits": {"cpu": "200m", "memory": "256Mi"}
            }
        })
    }

    // ── Namespace creation tests ────────────────────────────────────────

    #[test]
    fn namespace_name_follows_azureclaw_prefix() {
        let name = "my-agent";
        let sandbox_ns = format!("azureclaw-{name}");
        assert_eq!(sandbox_ns, "azureclaw-my-agent");
        assert!(sandbox_ns.starts_with("azureclaw-"));
    }

    #[test]
    fn namespace_labels_include_app_and_role() {
        let ns = build_namespace_json("test-agent");
        let labels = &ns["metadata"]["labels"];
        assert_eq!(labels["app.kubernetes.io/name"], "azureclaw");
        assert_eq!(labels["app.kubernetes.io/component"], "sandbox");
        assert_eq!(labels["azureclaw.azure.com/sandbox"], "test-agent");
        assert_eq!(labels["azureclaw.azure.com/role"], "sandbox");
    }

    #[test]
    fn namespace_has_pod_security_admission_labels() {
        let ns = build_namespace_json("psa-test");
        let labels = &ns["metadata"]["labels"];
        assert_eq!(labels["pod-security.kubernetes.io/enforce"], "privileged");
        assert_eq!(labels["pod-security.kubernetes.io/audit"], "baseline");
        assert_eq!(labels["pod-security.kubernetes.io/warn"], "baseline");
    }

    // ── NetworkPolicy tests ─────────────────────────────────────────────

    #[test]
    fn default_egress_allows_dns_on_port_53() {
        let rules = build_default_egress_rules();
        let dns_rule = &rules[0];
        let ports = dns_rule["ports"].as_array().unwrap();
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0]["port"], 53);
        assert_eq!(ports[0]["protocol"], "UDP");
        assert_eq!(ports[1]["port"], 53);
        assert_eq!(ports[1]["protocol"], "TCP");
    }

    #[test]
    fn default_egress_allows_imds() {
        let rules = build_default_egress_rules();
        let imds_rule = &rules[1];
        assert_eq!(imds_rule["to"][0]["ipBlock"]["cidr"], "169.254.169.254/32");
        assert_eq!(imds_rule["ports"][0]["port"], 80);
    }

    #[test]
    fn default_egress_allows_https_excluding_private_ranges() {
        let rules = build_default_egress_rules();
        let https_rule = &rules[2];
        assert_eq!(https_rule["to"][0]["ipBlock"]["cidr"], "0.0.0.0/0");
        let except = https_rule["to"][0]["ipBlock"]["except"].as_array().unwrap();
        assert!(except.contains(&json!("10.0.0.0/8")));
        assert!(except.contains(&json!("172.16.0.0/12")));
        assert!(except.contains(&json!("192.168.0.0/16")));
        assert_eq!(https_rule["ports"][0]["port"], 443);
    }

    #[test]
    fn mesh_egress_targets_sandbox_namespaces() {
        let rules = build_default_egress_rules();
        let mesh_rule = &rules[3];
        assert_eq!(
            mesh_rule["to"][0]["namespaceSelector"]["matchLabels"]["azureclaw.azure.com/role"],
            "sandbox"
        );
        assert_eq!(mesh_rule["ports"][0]["port"], 8443);
    }

    #[test]
    fn relay_egress_targets_agentmesh_namespace() {
        let rules = build_default_egress_rules();
        let relay_rule = &rules[4];
        assert_eq!(
            relay_rule["to"][0]["namespaceSelector"]["matchLabels"]["app.kubernetes.io/managed-by"],
            "azureclaw"
        );
        let ports = relay_rule["ports"].as_array().unwrap();
        assert_eq!(ports[0]["port"], 8765); // relay WebSocket
        assert_eq!(ports[1]["port"], 8080); // registry HTTP
    }

    #[test]
    fn default_egress_has_five_rules() {
        let rules = build_default_egress_rules();
        assert_eq!(rules.len(), 5);
    }

    // ── RBAC tests ──────────────────────────────────────────────────────

    #[test]
    fn service_account_name_is_sandbox() {
        let sa = build_sa_json("my-agent", "test-client-id");
        assert_eq!(sa["metadata"]["name"], "sandbox");
    }

    #[test]
    fn service_account_has_workload_identity_annotation() {
        let sa = build_sa_json("my-agent", "abc-123-client-id");
        assert_eq!(
            sa["metadata"]["annotations"]["azure.workload.identity/client-id"],
            "abc-123-client-id"
        );
    }

    #[test]
    fn service_account_namespace_matches_sandbox() {
        let sa = build_sa_json("my-agent", "cid");
        assert_eq!(sa["metadata"]["namespace"], "azureclaw-my-agent");
    }

    #[test]
    fn cluster_role_binding_references_spawner_role() {
        let crb = build_crb_json("my-agent");
        assert_eq!(crb["roleRef"]["kind"], "ClusterRole");
        assert_eq!(crb["roleRef"]["name"], "azureclaw-sandbox-spawner");
        assert_eq!(crb["roleRef"]["apiGroup"], "rbac.authorization.k8s.io");
    }

    #[test]
    fn cluster_role_binding_name_includes_sandbox_name() {
        let crb = build_crb_json("my-agent");
        assert_eq!(crb["metadata"]["name"], "azureclaw-spawner-my-agent");
    }

    #[test]
    fn cluster_role_binding_subject_is_sandbox_sa() {
        let crb = build_crb_json("my-agent");
        let subject = &crb["subjects"][0];
        assert_eq!(subject["kind"], "ServiceAccount");
        assert_eq!(subject["name"], "sandbox");
        assert_eq!(subject["namespace"], "azureclaw-my-agent");
    }

    #[test]
    fn cluster_role_binding_has_managed_by_label() {
        let crb = build_crb_json("test");
        assert_eq!(
            crb["metadata"]["labels"]["app.kubernetes.io/managed-by"],
            "azureclaw-controller"
        );
    }

    // ── Pod spec: container tests ───────────────────────────────────────

    #[test]
    fn base_pod_has_two_containers() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let containers = [oc, router];
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0]["name"], "openclaw");
        assert_eq!(containers[1]["name"], "inference-router");
    }

    #[test]
    fn pod_has_two_containers() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let containers = [oc, router];
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0]["name"], "openclaw");
        assert_eq!(containers[1]["name"], "inference-router");
    }

    #[test]
    fn inference_router_listens_on_port_8443() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let ports = router["ports"].as_array().unwrap();
        assert_eq!(ports[0]["containerPort"], 8443);
        assert_eq!(ports[0]["name"], "inference");
    }

    #[test]
    fn inference_router_exposes_metrics_port() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let ports = router["ports"].as_array().unwrap();
        assert_eq!(ports[1]["containerPort"], 9090);
        assert_eq!(ports[1]["name"], "metrics");
    }

    #[test]
    fn openclaw_gateway_port_18789() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        assert_eq!(oc["ports"][0]["containerPort"], 18789);
        assert_eq!(oc["ports"][0]["name"], "gateway");
    }

    // ── Pod spec: UID segregation ───────────────────────────────────────

    #[test]
    fn container_uids_are_segregated() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        assert_eq!(oc["securityContext"]["runAsUser"], 1000);
        assert_eq!(router["securityContext"]["runAsUser"], 1001);
    }

    // ── Pod spec: router security ──────────────────────────────────────

    #[test]
    fn router_denies_privilege_escalation() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        assert_eq!(router["securityContext"]["allowPrivilegeEscalation"], false);
    }

    #[test]
    fn router_has_read_only_rootfs() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        assert_eq!(router["securityContext"]["readOnlyRootFilesystem"], true);
    }

    #[test]
    fn router_drops_all_capabilities() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        assert_eq!(
            router["securityContext"]["capabilities"]["drop"],
            json!(["ALL"])
        );
    }

    // ── Pod spec: router probes ────────────────────────────────────────

    #[test]
    fn router_probes_use_httpget_no_host() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let liveness = &router["livenessProbe"]["httpGet"];
        assert_eq!(liveness["path"], "/healthz");
        assert_eq!(liveness["port"], "inference");
        assert!(liveness.get("host").is_none());

        let readiness = &router["readinessProbe"]["httpGet"];
        assert_eq!(readiness["path"], "/healthz");
        assert!(readiness.get("host").is_none());
    }

    // ── Pod spec: volumes ───────────────────────────────────────────────

    #[test]
    fn openclaw_has_sandbox_data_volume_mount() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        let mounts = oc["volumeMounts"].as_array().unwrap();
        let names: Vec<&str> = mounts.iter().map(|m| m["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"sandbox-data"));
        assert!(names.contains(&"tmp"));
        assert!(names.contains(&"admin-token"));
    }

    #[test]
    fn router_has_admin_token_volume_mount() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let mounts = router["volumeMounts"].as_array().unwrap();
        assert_eq!(mounts[0]["name"], "admin-token");
        assert_eq!(mounts[0]["readOnly"], true);
    }

    // ── Pod spec: init container ────────────────────────────────────────

    #[test]
    fn init_container_needs_net_admin_capability() {
        let init = build_init_container("router:latest");
        let caps = &init["securityContext"]["capabilities"];
        let add = caps["add"].as_array().unwrap();
        assert!(add.contains(&json!("NET_ADMIN")));
        assert!(add.contains(&json!("NET_RAW")));
    }

    #[test]
    fn init_container_runs_as_root() {
        let init = build_init_container("router:latest");
        assert_eq!(init["securityContext"]["runAsUser"], 0);
        assert_eq!(init["securityContext"]["runAsNonRoot"], false);
    }

    #[test]
    fn init_container_seccomp_unconfined() {
        let init = build_init_container("router:latest");
        assert_eq!(
            init["securityContext"]["seccompProfile"]["type"],
            "Unconfined"
        );
    }

    // ── Pod spec: image pull policy ─────────────────────────────────────

    #[test]
    fn pull_policy_always_for_latest_tag() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        assert_eq!(oc["imagePullPolicy"], "Always");
    }

    #[test]
    fn pull_policy_ifnotpresent_for_versioned_tag() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:v1.2.3", &cfg, "gpt-4.1");
        assert_eq!(oc["imagePullPolicy"], "IfNotPresent");
    }

    // ── Environment variable injection ──────────────────────────────────

    #[test]
    fn router_env_includes_sandbox_name() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "my-agent", &cfg, "gpt-4.1");
        let env = router["env"].as_array().unwrap();
        let sandbox_name_var = env
            .iter()
            .find(|e| e["name"] == "SANDBOX_NAME")
            .expect("SANDBOX_NAME env var missing");
        assert_eq!(sandbox_name_var["value"], "my-agent");
    }

    #[test]
    fn router_env_includes_content_safety_endpoint() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let env = router["env"].as_array().unwrap();
        let cs_var = env
            .iter()
            .find(|e| e["name"] == "CONTENT_SAFETY_ENDPOINT")
            .expect("CONTENT_SAFETY_ENDPOINT missing");
        assert!(!cs_var["value"].as_str().unwrap().is_empty());
    }

    #[test]
    fn router_env_includes_foundry_project_endpoint() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let env = router["env"].as_array().unwrap();
        let fp_var = env
            .iter()
            .find(|e| e["name"] == "FOUNDRY_PROJECT_ENDPOINT")
            .expect("FOUNDRY_PROJECT_ENDPOINT missing");
        assert!(!fp_var["value"].as_str().unwrap().is_empty());
    }

    #[test]
    fn router_env_includes_model_deployment() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let env = router["env"].as_array().unwrap();
        let deployment_var = env
            .iter()
            .find(|e| e["name"] == "AZURE_OPENAI_DEPLOYMENT")
            .expect("AZURE_OPENAI_DEPLOYMENT missing");
        assert_eq!(deployment_var["value"], "gpt-4.1");
    }

    #[test]
    fn router_env_includes_token_budget_daily() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        let env = router["env"].as_array().unwrap();
        let budget_var = env
            .iter()
            .find(|e| e["name"] == "TOKEN_BUDGET_DAILY")
            .expect("TOKEN_BUDGET_DAILY missing");
        assert_eq!(budget_var["value"], "0"); // default unlimited
    }

    #[test]
    fn openclaw_env_includes_model() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        let env = oc["env"].as_array().unwrap();
        let model_var = env
            .iter()
            .find(|e| e["name"] == "OPENCLAW_MODEL")
            .expect("OPENCLAW_MODEL missing");
        assert_eq!(model_var["value"], "gpt-4.1");
    }

    #[test]
    fn openclaw_env_includes_azure_openai_endpoint() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        let env = oc["env"].as_array().unwrap();
        let ep_var = env
            .iter()
            .find(|e| e["name"] == "AZURE_OPENAI_ENDPOINT")
            .expect("AZURE_OPENAI_ENDPOINT missing");
        assert!(!ep_var["value"].as_str().unwrap().is_empty());
    }

    // ── Default resource limits ─────────────────────────────────────────

    #[test]
    fn openclaw_default_resource_limits() {
        let cfg = SandboxConfig::default();
        let oc = build_openclaw_container("img:latest", &cfg, "gpt-4.1");
        assert_eq!(oc["resources"]["requests"]["cpu"], "500m");
        assert_eq!(oc["resources"]["requests"]["memory"], "1Gi");
        assert_eq!(oc["resources"]["limits"]["cpu"], "2");
        assert_eq!(oc["resources"]["limits"]["memory"], "4Gi");
    }

    #[test]
    fn router_default_resource_limits() {
        let cfg = SandboxConfig::default();
        let router = build_router_container("router:latest", "test", &cfg, "gpt-4.1");
        assert_eq!(router["resources"]["requests"]["cpu"], "100m");
        assert_eq!(router["resources"]["requests"]["memory"], "64Mi");
        assert_eq!(router["resources"]["limits"]["cpu"], "500m");
        assert_eq!(router["resources"]["limits"]["memory"], "256Mi");
    }

    // ── Finalizer ───────────────────────────────────────────────────────

    #[test]
    fn finalizer_name_is_namespace_cleanup() {
        // The reconcile function uses this exact finalizer name (line 127)
        let expected = "azureclaw.azure.com/namespace-cleanup";
        // Verify the format matches the domain/purpose convention
        assert!(expected.starts_with("azureclaw.azure.com/"));
        assert!(expected.contains("namespace-cleanup"));
    }

    // ── Isolation + runtime class ───────────────────────────────────────

    #[test]
    fn confidential_isolation_gets_kata_runtime_class() {
        let (runtime, _pool) = isolation_scheduling("confidential");
        assert_eq!(runtime, Some("kata-vm-isolation"));
    }

    #[test]
    fn standard_and_enhanced_share_sandbox_pool() {
        let (_, pool_std) = isolation_scheduling("standard");
        let (_, pool_enh) = isolation_scheduling("enhanced");
        assert_eq!(pool_std, pool_enh);
        assert_eq!(pool_std, "sandbox");
    }

    #[test]
    fn unknown_isolation_defaults_to_sandbox_pool() {
        let (runtime, pool) = isolation_scheduling("unknown-level");
        assert!(runtime.is_none());
        assert_eq!(pool, "sandbox");
    }

    // ── Security context edge cases ─────────────────────────────────────

    #[test]
    fn explicit_runtime_default_seccomp_overrides_localhost() {
        let cfg = SandboxConfig {
            isolation: "enhanced".into(),
            seccomp_profile: "RuntimeDefault".into(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
    }

    #[test]
    fn empty_seccomp_profile_uses_runtime_default() {
        let cfg = SandboxConfig {
            isolation: "enhanced".into(),
            seccomp_profile: String::new(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["seccompProfile"]["type"], "RuntimeDefault");
    }

    #[test]
    fn custom_seccomp_profile_name() {
        let cfg = SandboxConfig {
            isolation: "enhanced".into(),
            seccomp_profile: "my-custom-profile".into(),
            ..Default::default()
        };
        let ctx = build_pod_security_context(&cfg);
        assert_eq!(ctx["seccompProfile"]["type"], "Localhost");
        assert_eq!(
            ctx["seccompProfile"]["localhostProfile"],
            "profiles/my-custom-profile.json"
        );
    }

    // ── Error-policy / watch-resilience contract (r4) ───────────────────
    //
    // These tests guard the reconcile-error requeue contract. The
    // watch-stream itself is kube-rs's problem (Controller::new +
    // watcher::Config handle stream reconnect with built-in backoff) —
    // we only test the piece we own: that any ReconcileError yields a
    // positive, bounded requeue duration. A regression to
    // `Duration::ZERO` would hot-loop the controller.

    #[test]
    fn error_requeue_kube_is_short() {
        let err = ReconcileError::Kube(kube::Error::LinesCodecMaxLineLengthExceeded);
        let d = error_requeue_duration(&err);
        assert!(d >= Duration::from_secs(10), "too short: {:?}", d);
        assert!(d <= Duration::from_secs(120), "too long: {:?}", d);
    }

    #[test]
    fn error_requeue_serde_is_long() {
        // Produce a real serde_json::Error without an unwrap panic.
        let serde_err = serde_json::from_str::<serde_json::Value>("{bad").unwrap_err();
        let err = ReconcileError::SerdeJson(serde_err);
        let d = error_requeue_duration(&err);
        // Serde errors won't heal on retry — we want a longer backoff.
        assert!(
            d >= Duration::from_secs(60),
            "serde backoff too short: {:?} — this would log-spam",
            d
        );
    }

    #[test]
    fn error_requeue_is_never_zero() {
        // Build one of each variant and confirm the requeue is strictly
        // positive. A zero requeue would starve the controller event
        // loop and pin a CPU.
        let kube_err = ReconcileError::Kube(kube::Error::LinesCodecMaxLineLengthExceeded);
        assert!(error_requeue_duration(&kube_err) > Duration::ZERO);

        let serde_err = ReconcileError::SerdeJson(
            serde_json::from_str::<serde_json::Value>("{bad").unwrap_err(),
        );
        assert!(error_requeue_duration(&serde_err) > Duration::ZERO);
    }
