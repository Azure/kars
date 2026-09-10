// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::mcp_server::{ManagedMcpPreset, McpServer};
use kube::ResourceExt;
use serde_json::{Value, json};

pub(super) const SOURCE_NS: &str = "kars.azure.com/mcp-source-namespace";
pub(super) const SOURCE_NAME: &str = "kars.azure.com/mcp-source-name";
pub(super) const SOURCE_UID: &str = "kars.azure.com/mcp-source-uid";
pub(super) const NAMESPACE_UID: &str = "kars.azure.com/mcp-namespace-uid";
pub(super) const CONTROLLER_NS: &str = "kars.azure.com/mcp-controller-namespace";
pub(super) const CONTROLLER_UID: &str = "kars.azure.com/mcp-controller-namespace-uid";
pub(super) const CLAIM: &str = "kars.azure.com/mcp-namespace-claim";

const PLAYWRIGHT_IMAGE: &str = "mcr.microsoft.com/playwright/mcp@sha256:3d871c22ea2d4cca0966e2cfb1860e1cb03eb7353725a3d6cffd133296fb04eb";
const EVERYTHING_IMAGE: &str = "ghcr.io/azure/kars/mcp-everything:latest";

#[derive(Clone, Debug)]
pub(super) struct Config {
    pub controller_namespace: String,
    pub namespace: String,
    pub playwright_image: String,
    pub everything_image: String,
    pub pull_secret: Option<String>,
}

pub(super) fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
}

impl Config {
    pub(super) fn from_env() -> Result<Self, String> {
        let value = |key: &str, default: &str| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| default.into())
        };
        let config = Self {
            controller_namespace: crate::providers::signing::receipt_namespace(),
            namespace: value("MCP_MANAGED_NAMESPACE", "kars-mcp"),
            playwright_image: value("MCP_PLAYWRIGHT_IMAGE", PLAYWRIGHT_IMAGE),
            everything_image: value("MCP_EVERYTHING_IMAGE", EVERYTHING_IMAGE),
            pull_secret: std::env::var("MCP_IMAGE_PULL_SECRET_NAME")
                .ok()
                .filter(|value| !value.is_empty()),
        };
        config.validate()?;
        Ok(config)
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if !dns_label(&self.controller_namespace)
            || !dns_label(&self.namespace)
            || self.namespace == self.controller_namespace
            || [
                "default",
                "kube-system",
                "kube-public",
                "kube-node-lease",
                "kars-sre",
                "agentmesh",
            ]
            .contains(&self.namespace.as_str())
            || self
                .pull_secret
                .as_ref()
                .is_some_and(|name| !dns_label(name))
        {
            return Err(
                "Managed MCP requires a separate valid namespace and a local pull Secret name"
                    .into(),
            );
        }
        for image in [&self.playwright_image, &self.everything_image] {
            if image.len() > 512
                || image.chars().any(char::is_whitespace)
                || (!image.contains("@sha256:")
                    && image
                        .rfind(':')
                        .is_none_or(|index| image.rfind('/').is_some_and(|slash| index < slash)))
            {
                return Err("Managed MCP operator images require an explicit tag or digest".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(super) struct Owner {
    pub namespace: String,
    pub name: String,
    pub source_namespace: String,
    pub source_name: String,
    pub source_uid: String,
}

impl Owner {
    pub(super) fn new(mcp: &McpServer, namespace: &str) -> Result<Self, String> {
        let source_namespace = mcp
            .namespace()
            .filter(|ns| dns_label(ns))
            .ok_or("McpServer namespace is invalid")?;
        let source_uid = mcp
            .uid()
            .filter(|uid| {
                !uid.is_empty()
                    && uid.len() <= 63
                    && uid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            .ok_or("Managed MCP requires a live source UID")?;
        let source_name = mcp
            .metadata
            .name
            .clone()
            .filter(|name| !name.is_empty() && name.len() <= 253 && name.split('.').all(dns_label))
            .ok_or("McpServer source name is invalid")?;
        if !dns_label(namespace) {
            return Err("Managed MCP namespace is invalid".into());
        }
        let identity = format!("{source_namespace}/{source_uid}");
        let digest = crate::providers::signing::content_digest(identity.as_bytes());
        let suffix = &digest.trim_start_matches("sha256:")[..20];
        let display: String = source_name
            .chars()
            .take(38)
            .map(|ch| if ch == '.' { '-' } else { ch })
            .collect();
        let name = format!("mcp-{}-{suffix}", display.trim_matches('-'));
        if !dns_label(&name) {
            return Err("Managed MCP workload name is invalid".into());
        }
        Ok(Self {
            namespace: namespace.into(),
            name,
            source_namespace,
            source_name,
            source_uid,
        })
    }

    pub(super) fn workload_ref(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    pub(super) fn labels(&self) -> Value {
        json!({"app.kubernetes.io/name":self.name, "app.kubernetes.io/component":"mcp-server",
            "app.kubernetes.io/managed-by":"kars-controller",SOURCE_UID:self.source_uid})
    }

    pub(super) fn annotations(&self, namespace_uid: &str) -> Value {
        json!({SOURCE_NS:self.source_namespace,SOURCE_NAME:self.source_name,SOURCE_UID:self.source_uid,
            NAMESPACE_UID:namespace_uid})
    }
}

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub owner: Owner,
    pub image: String,
    pub port: u16,
    pub args: Vec<String>,
    pub cpu_request: &'static str,
    pub memory_request: &'static str,
    pub cpu_limit: &'static str,
    pub memory_limit: &'static str,
}

impl Plan {
    pub(super) fn new(mcp: &McpServer, config: &Config) -> Result<Self, String> {
        config.validate()?;
        let owner = Owner::new(mcp, &config.namespace)?;
        if !dns_label(&owner.source_name) {
            return Err("Managed MCP names must be DNS labels for namespaced tool dispatch".into());
        }
        let managed = mcp
            .spec
            .managed
            .as_ref()
            .ok_or("McpServer has no managed preset")?;
        let (image, port, args, cpu_request, memory_request, cpu_limit, memory_limit) =
            match managed.preset {
                ManagedMcpPreset::Playwright => {
                    let host =
                        format!("{}.{}.svc.cluster.local:8931", owner.name, config.namespace);
                    (
                        config.playwright_image.clone(),
                        8931,
                        vec![
                            "--port=8931".into(),
                            "--host=0.0.0.0".into(),
                            "--headless".into(),
                            "--browser=chromium".into(),
                            "--no-sandbox".into(),
                            "--isolated".into(),
                            format!("--allowed-hosts={host},localhost:8931,localhost"),
                        ],
                        "250m",
                        "512Mi",
                        "2",
                        "2Gi",
                    )
                }
                ManagedMcpPreset::Everything => (
                    config.everything_image.clone(),
                    3001,
                    vec!["streamableHttp".into()],
                    "50m",
                    "128Mi",
                    "500m",
                    "512Mi",
                ),
            };
        Ok(Self {
            owner,
            image,
            port,
            args,
            cpu_request,
            memory_request,
            cpu_limit,
            memory_limit,
        })
    }

    pub(super) fn endpoint(&self) -> String {
        format!(
            "http://{}.{}.svc.cluster.local:{}/mcp",
            self.owner.name, self.owner.namespace, self.port
        )
    }

    pub(super) fn deployment(&self, namespace_uid: &str, pull_secret: Option<&str>) -> Value {
        json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":self.owner.name,"namespace":self.owner.namespace,"labels":self.owner.labels(),"annotations":self.owner.annotations(namespace_uid)},
            "spec":{"replicas":1,"strategy":{"type":"Recreate"},
                "selector":{"matchLabels":{SOURCE_UID:self.owner.source_uid}},
                "template":{"metadata":{"labels":self.owner.labels(),"annotations":self.owner.annotations(namespace_uid)},
                    "spec":{"automountServiceAccountToken":false,
                        "securityContext":{"runAsNonRoot":true,"runAsUser":1000,"runAsGroup":1000,"fsGroup":1000,
                            "seccompProfile":{"type":"RuntimeDefault"}},
                        "imagePullSecrets":pull_secret.map(|name|vec![json!({"name":name})]).unwrap_or_default(),
                        "containers":[{"name":"mcp","image":self.image,
                            "imagePullPolicy":if self.image.ends_with(":latest") {"Always"} else {"IfNotPresent"},
                            "args":self.args,"env":[{"name":"PORT","value":self.port.to_string()},{"name":"HOME","value":"/tmp"},{"name":"TMPDIR","value":"/tmp"}],
                            "ports":[{"name":"mcp","containerPort":self.port}],
                            "readinessProbe":{"tcpSocket":{"port":"mcp"},"initialDelaySeconds":3,"periodSeconds":5},
                            "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
                            "resources":{"requests":{"cpu":self.cpu_request,"memory":self.memory_request},
                                "limits":{"cpu":self.cpu_limit,"memory":self.memory_limit}},
                            "volumeMounts":[{"name":"scratch","mountPath":"/tmp"}]}],
                        "volumes":[{"name":"scratch","emptyDir":{"sizeLimit":"1Gi"}}]}}}})
    }

    pub(super) fn service(&self, namespace_uid: &str) -> Value {
        json!({"apiVersion":"v1","kind":"Service",
            "metadata":{"name":self.owner.name,"namespace":self.owner.namespace,"labels":self.owner.labels(),"annotations":self.owner.annotations(namespace_uid)},
            "spec":{"type":"ClusterIP","selector":{SOURCE_UID:self.owner.source_uid},
                "ports":[{"name":"mcp","port":self.port,"targetPort":"mcp"}]}})
    }

    pub(super) fn network_policy(&self, namespace_uid: &str, peers: Vec<Value>) -> Value {
        json!({"apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
        "metadata":{"name":self.owner.name,"namespace":self.owner.namespace,"labels":self.owner.labels(),"annotations":self.owner.annotations(namespace_uid)},
        "spec":{"podSelector":{"matchLabels":{SOURCE_UID:self.owner.source_uid}},"policyTypes":["Ingress","Egress"],
            "ingress":[{"from":peers,"ports":[{"protocol":"TCP","port":self.port}]}],
            "egress":[
                {"to":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":"kube-system"}}}],
                    "ports":[{"protocol":"UDP","port":53},{"protocol":"TCP","port":53}]},
                {"to":[{"ipBlock":{"cidr":"0.0.0.0/0","except":["0.0.0.0/8","10.0.0.0/8","100.64.0.0/10",
                    "127.0.0.0/8","169.254.0.0/16","172.16.0.0/12","192.168.0.0/16","224.0.0.0/4","240.0.0.0/4"]}}],
                    "ports":[{"protocol":"TCP","port":80},{"protocol":"TCP","port":443}]}
            ]}})
    }
}
