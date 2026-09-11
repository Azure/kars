use super::ComposeEgress;

pub(super) fn complete_egress_recommendation(
    egress: Vec<ComposeEgress>,
    intent: &str,
    mcp_servers: &[String],
) -> Vec<ComposeEgress> {
    let mut endpoints = std::collections::BTreeMap::<String, Option<u16>>::new();
    let lower = intent.to_ascii_lowercase();
    let npm_intent = ["npm", "node", "javascript", "typescript", "package.json"]
        .iter()
        .any(|term| lower.contains(term));
    let python_intent = ["python", "pip", "pypi", "requirements.txt"]
        .iter()
        .any(|term| lower.contains(term));
    for endpoint in egress {
        let host = endpoint.host.to_ascii_lowercase();
        if host.contains("githubcopilot.com")
            || host.ends_with(".openai.azure.com")
            || host.ends_with(".services.ai.azure.com")
        {
            continue;
        }
        if host == "registry.npmjs.org" && !npm_intent {
            continue;
        }
        if matches!(host.as_str(), "pypi.org" | "files.pythonhosted.org") && !python_intent {
            continue;
        }
        endpoints.insert(host, endpoint.port.or(Some(443)));
    }
    let github = mcp_servers
        .iter()
        .any(|server| server.to_ascii_lowercase().contains("github"))
        || [
            "github",
            "repository",
            "pull request",
            "dependabot",
            "code scanning",
        ]
        .iter()
        .any(|term| lower.contains(term));
    let mut add = |host: &str| {
        endpoints.entry(host.to_string()).or_insert(Some(443));
    };
    if github {
        for host in [
            "api.github.com",
            "github.com",
            "raw.githubusercontent.com",
            "codeload.github.com",
            "objects.githubusercontent.com",
            "patch-diff.githubusercontent.com",
        ] {
            add(host);
        }
    }
    if npm_intent {
        add("registry.npmjs.org");
    }
    if python_intent {
        add("pypi.org");
        add("files.pythonhosted.org");
    }
    if [
        "security advisory",
        "security advisories",
        "vulnerability",
        "vulnerabilities",
        "cve",
    ]
    .iter()
    .any(|term| lower.contains(term))
    {
        add("api.osv.dev");
    }
    if ["rust", "cargo", "crates.io"]
        .iter()
        .any(|term| lower.contains(term))
    {
        add("index.crates.io");
        add("static.crates.io");
    }
    endpoints
        .into_iter()
        .map(|(host, port)| ComposeEgress { host, port })
        .collect()
}
