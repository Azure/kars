// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! MCP egress derivation.
//!
//! "Apply an `McpServer` and reference it — it just works." The per-pod
//! inference router is the only network path to an MCP server, so the sandbox's
//! default-deny `NetworkPolicy` must admit the router→server hop without the
//! operator also hand-writing a `networkPolicy.allowedEndpoints` entry. These
//! helpers turn an `McpServer.spec.url` into the matching egress rule; the
//! reconciler walks every referenced server and adds the derived rules.

use serde_json::json;

/// Auto-derive the NetworkPolicy egress rules that admit the sandbox router to
/// every `McpServer` the sandbox references. For each referent we fetch its
/// `spec.url`, parse it, and build the matching rule (see [`mcp_egress_rule`]),
/// skipping any that duplicate `existing` rules or each other.
///
/// A missing (`404`) referent is logged and skipped — the JWKS-mirror path
/// surfaces it elsewhere. Any other API error is returned so the caller can
/// requeue. Empty / unparseable URLs are skipped (those endpoints still need an
/// explicit allowlist entry).
pub(crate) async fn derive_mcp_egress_rules(
    client: &kube::Client,
    sandbox_self_ns: &str,
    refs: &[&crate::mcp_server::LocalObjectRef],
    sandbox_name: &str,
    existing: &[serde_json::Value],
) -> Result<Vec<serde_json::Value>, kube::Error> {
    use kube::api::Api;
    let mcp_api: Api<crate::mcp_server::McpServer> =
        Api::namespaced(client.clone(), sandbox_self_ns);
    let mut out: Vec<serde_json::Value> = Vec::new();
    for mcp_ref in refs {
        let ref_name = mcp_ref.name.trim();
        if ref_name.is_empty() {
            continue;
        }
        let url = match mcp_api.get(ref_name).await {
            Ok(mcp) => mcp.spec.url.unwrap_or_default(),
            Err(kube::Error::Api(ae)) if ae.code == 404 => {
                tracing::warn!(
                    sandbox = %sandbox_name,
                    mcp = %ref_name,
                    "referenced McpServer not found; egress not auto-derived",
                );
                continue;
            }
            Err(e) => return Err(e),
        };
        if url.is_empty() {
            continue;
        }
        let Some((host, port)) = mcp_url_host_port(&url) else {
            tracing::warn!(
                sandbox = %sandbox_name,
                mcp = %ref_name,
                url = %url,
                "McpServer url not parseable for egress derivation; skipping",
            );
            continue;
        };
        if let Some(rule) = mcp_egress_rule(&host, port)
            && !existing.contains(&rule)
            && !out.contains(&rule)
        {
            tracing::info!(
                sandbox = %sandbox_name,
                mcp = %ref_name,
                host = %host,
                port,
                "auto-derived MCP egress rule",
            );
            out.push(rule);
        }
    }
    Ok(out)
}

/// Parse an `McpServer.spec.url` into the `(host, port)` the sandbox's
/// inference-router must be allowed to reach.
///
/// Returns `None` when the URL is empty or unparseable (the caller skips
/// egress derivation and lets the router surface the misconfiguration).
/// The port defaults from the scheme (`https` → 443, `http` → 80) when not
/// explicitly given. Only `http`/`https` MCP transports are supported.
///
/// Examples:
/// - `https://mcp.deepwiki.com/mcp` → `("mcp.deepwiki.com", 443)`
/// - `http://playwright-mcp.default.svc.cluster.local:8931/mcp`
///   → `("playwright-mcp.default.svc.cluster.local", 8931)`
pub(crate) fn mcp_url_host_port(url: &str) -> Option<(String, u16)> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let default_port: u16 = match scheme.to_ascii_lowercase().as_str() {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    // Strip path / query / fragment — everything from the first '/', '?' or '#'.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("").trim();
    if authority.is_empty() {
        return None;
    }
    // Drop any userinfo ("user:pass@host").
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 literal `[::1]:8931` — not expected for MCP services; reject so the
    // caller falls back rather than mis-splitting on the inner colons.
    if host_port.starts_with('[') {
        return None;
    }
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => {
            let port = p.parse::<u16>().ok()?;
            (h, port)
        }
        None => (host_port, default_port),
    };
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

/// Build the NetworkPolicy egress rule that admits the sandbox router to an
/// MCP server at `host:port`. In-cluster Service DNS names get a
/// `namespaceSelector` rule (the only form Cilium honours for pod
/// destinations); external hosts get the coarse port-level `ipBlock`
/// (host-level enforcement lives in the router). Returns `None` for an
/// external host on 443 — already covered by the blanket HTTPS rule.
pub(crate) fn mcp_egress_rule(host: &str, port: u16) -> Option<serde_json::Value> {
    if let Some(ns) = cluster_internal_namespace(host) {
        Some(json!({
            "to": [{
                "namespaceSelector": {
                    "matchLabels": { "kubernetes.io/metadata.name": ns }
                }
            }],
            "ports": [{"protocol": "TCP", "port": port}]
        }))
    } else if port == 443 {
        None
    } else {
        Some(json!({
            "to": [{"ipBlock": {"cidr": "0.0.0.0/0"}}],
            "ports": [{"protocol": "TCP", "port": port}]
        }))
    }
}

/// If `host` is a cluster-internal Kubernetes DNS name, return the namespace
/// it resolves into; otherwise `None`.
///
/// In-cluster MCP servers / egress endpoints are addressed by their Service
/// DNS name — `<svc>.<ns>.svc.cluster.local` (or the `.svc` short form). For
/// those we MUST express the NetworkPolicy egress with a `namespaceSelector`,
/// not an `ipBlock`: under the Cilium CNI a K8s NetworkPolicy `ipBlock` CIDR
/// (including `0.0.0.0/0`) only matches the reserved `world` entity and is
/// NOT applied to in-cluster pod endpoints, so an `ipBlock`-based rule silently
/// fails to admit traffic to another pod. Selecting the destination namespace
/// by its well-known `kubernetes.io/metadata.name` label is the portable way to
/// open egress to an in-cluster Service.
///
/// Returns `None` for external hostnames (e.g. `api.openai.com`) and for bare,
/// namespace-ambiguous names — those keep the coarse `ipBlock` port rule, with
/// fine-grained host enforcement handled by the router's CONNECT allowlist.
pub(crate) fn cluster_internal_namespace(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.');
    // `<svc>.<ns>.svc.cluster.local` or the cluster-suffixless `<svc>.<ns>.svc`.
    let stripped = host
        .strip_suffix(".svc.cluster.local")
        .or_else(|| host.strip_suffix(".svc"))?;
    // After stripping the `.svc[.cluster.local]` suffix, the remainder is
    // `<svc>.<ns>`; the namespace is the last label.
    let ns = stripped.rsplit('.').next()?;
    if ns.is_empty() || ns == stripped {
        // `stripped` had no `.` → no namespace label present.
        return None;
    }
    Some(ns.to_string())
}

#[cfg(test)]
mod tests {
    use super::{cluster_internal_namespace, mcp_egress_rule, mcp_url_host_port};

    #[test]
    fn cluster_internal_namespace_parses_fqdn_forms() {
        assert_eq!(
            cluster_internal_namespace("playwright-mcp.default.svc.cluster.local"),
            Some("default".to_string())
        );
        assert_eq!(
            cluster_internal_namespace("playwright-mcp.default.svc"),
            Some("default".to_string())
        );
        // Trailing dot (fully-qualified) is tolerated.
        assert_eq!(
            cluster_internal_namespace("svc-x.team-a.svc.cluster.local."),
            Some("team-a".to_string())
        );
    }

    #[test]
    fn cluster_internal_namespace_rejects_external_and_ambiguous() {
        // External hostnames keep the coarse ipBlock rule.
        assert_eq!(cluster_internal_namespace("api.openai.com"), None);
        assert_eq!(cluster_internal_namespace("example.com"), None);
        // Bare names without a namespace label are ambiguous → None.
        assert_eq!(cluster_internal_namespace("playwright-mcp"), None);
        // A `.svc` suffix with no namespace label is not a valid in-cluster name.
        assert_eq!(cluster_internal_namespace("svc"), None);
    }

    #[test]
    fn mcp_url_host_port_parses_common_forms() {
        // Remote HTTPS MCP — default port 443.
        assert_eq!(
            mcp_url_host_port("https://mcp.deepwiki.com/mcp"),
            Some(("mcp.deepwiki.com".to_string(), 443))
        );
        assert_eq!(
            mcp_url_host_port("https://api.githubcopilot.com/mcp"),
            Some(("api.githubcopilot.com".to_string(), 443))
        );
        // In-cluster HTTP MCP with explicit port.
        assert_eq!(
            mcp_url_host_port("http://playwright-mcp.default.svc.cluster.local:8931/mcp"),
            Some(("playwright-mcp.default.svc.cluster.local".to_string(), 8931))
        );
        // http default port 80, no path.
        assert_eq!(
            mcp_url_host_port("http://svc.ns.svc.cluster.local"),
            Some(("svc.ns.svc.cluster.local".to_string(), 80))
        );
        // Query/fragment are stripped.
        assert_eq!(
            mcp_url_host_port("https://host.example:8443/mcp?x=1#frag"),
            Some(("host.example".to_string(), 8443))
        );
    }

    #[test]
    fn mcp_url_host_port_rejects_invalid() {
        assert_eq!(mcp_url_host_port(""), None);
        assert_eq!(mcp_url_host_port("not-a-url"), None);
        assert_eq!(mcp_url_host_port("ftp://host:21/x"), None);
        assert_eq!(mcp_url_host_port("https://"), None);
        // Non-numeric port.
        assert_eq!(mcp_url_host_port("https://host:notaport/mcp"), None);
        // IPv6 literals are not supported (fall back rather than mis-split).
        assert_eq!(mcp_url_host_port("http://[::1]:8931/mcp"), None);
    }

    #[test]
    fn mcp_egress_rule_shapes_by_destination() {
        // In-cluster → namespaceSelector on the destination namespace + port.
        let rule = mcp_egress_rule("playwright-mcp.default.svc.cluster.local", 8931)
            .expect("in-cluster rule");
        assert_eq!(
            rule["to"][0]["namespaceSelector"]["matchLabels"]["kubernetes.io/metadata.name"],
            "default"
        );
        assert_eq!(rule["ports"][0]["port"], 8931);

        // External non-443 → coarse ipBlock + port.
        let rule = mcp_egress_rule("mcp.example.com", 8080).expect("external non-443 rule");
        assert_eq!(rule["to"][0]["ipBlock"]["cidr"], "0.0.0.0/0");
        assert_eq!(rule["ports"][0]["port"], 8080);

        // External 443 → None (blanket HTTPS rule already covers it).
        assert!(mcp_egress_rule("mcp.deepwiki.com", 443).is_none());
    }
}
