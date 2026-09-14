// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::github_app::{API, GIT, repository};
use axum::http::{Method, Uri};

pub(super) struct Target {
    pub repo: String,
    pub url: String,
    pub git: bool,
    pub logs: bool,
}

fn numeric(value: &str) -> bool {
    value.len() <= 20 && value.parse::<u64>().ok().is_some_and(|number| number > 0)
}

/// A deliberately bounded REST surface, not a general credential injector.
fn api_allowed(method: &Method, rest: &[&str], write: bool) -> bool {
    if *method == Method::GET {
        return matches!(
            rest,
            [] | ["pulls"] | ["issues"] | ["commits"] | ["branches"] | ["tags"]
        ) || matches!(rest, ["pulls" | "issues", id] if numeric(id))
            || matches!(rest, ["pulls", id, "files" | "commits" | "reviews"] if numeric(id))
            || matches!(rest, ["issues", id, "comments"] if numeric(id))
            || matches!(rest, ["commits", reference] | ["commits", reference, "status" | "check-runs"]
                if !reference.is_empty())
            || matches!(rest, ["actions", "runs" | "workflows"])
            || matches!(rest, ["actions", "runs" | "jobs", id] if numeric(id))
            || matches!(rest, ["actions", "runs", id, "jobs"] if numeric(id))
            || matches!(rest, ["actions", "jobs", id, "logs"] if numeric(id))
            || matches!(rest, ["check-runs", id] if numeric(id));
    }
    write
        && *method == Method::POST
        && (matches!(rest, ["pulls"] | ["issues"])
            || matches!(rest, ["issues", id, "comments"] if numeric(id)))
}

pub(super) fn target(uri: &Uri, method: &Method, write: bool) -> Option<Target> {
    if uri.scheme().is_some() || uri.authority().is_some() || uri.to_string().len() > 4096 {
        return None;
    }
    let path = uri.path();
    // Percent encodings are unnecessary on this API subset. Reject them rather
    // than depending on multiple HTTP stacks to agree on recursive decoding.
    if path.contains('%')
        || path.contains('\\')
        || path.contains("//")
        || path.split('/').any(|segment| matches!(segment, "." | ".."))
        || !path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
    {
        return None;
    }
    let (git, prefix, base) = if path.starts_with("/git/") {
        (true, "/git/", GIT)
    } else {
        (false, "/gh-api/repos/", API)
    };
    let segments: Vec<_> = path.strip_prefix(prefix)?.split('/').collect();
    if segments.len() < 2 {
        return None;
    }
    let repo_name = if git {
        segments[1].strip_suffix(".git").unwrap_or(segments[1])
    } else {
        segments[1]
    };
    let repo = repository(&format!("{}/{}", segments[0], repo_name))?;
    let rest = &segments[2..];
    let query = uri.query();
    if git {
        let valid = match (method, rest, query) {
            (&Method::GET, ["info", "refs"], Some("service=git-upload-pack")) => true,
            (&Method::GET, ["info", "refs"], Some("service=git-receive-pack")) => write,
            (&Method::POST, ["git-upload-pack"], None) => true,
            (&Method::POST, ["git-receive-pack"], None) => write,
            _ => false,
        };
        if !valid {
            return None;
        }
    } else {
        if !api_allowed(method, rest, write) {
            return None;
        }
        if let Some(query) = query {
            if *method != Method::GET || !safe_query(query) {
                return None;
            }
        }
    }
    let logs = !git && matches!(rest, ["actions", "jobs", _, "logs"]);
    if logs && query.is_some() {
        return None;
    }
    let suffix = rest.join("/");
    let path = if git {
        format!("{repo}.git/{suffix}")
    } else if suffix.is_empty() {
        format!("repos/{repo}")
    } else {
        format!("repos/{repo}/{suffix}")
    };
    let url = match query {
        Some(query) => format!("{base}/{path}?{query}"),
        None => format!("{base}/{path}"),
    };
    Some(Target {
        repo,
        url,
        git,
        logs,
    })
}

fn safe_query(query: &str) -> bool {
    if query.len() > 1024 {
        return false;
    }
    let mut seen = std::collections::BTreeSet::new();
    query.split('&').all(|pair| {
        let Some((key, value)) = pair.split_once('=') else {
            return false;
        };
        if !seen.insert(key)
            || value.is_empty()
            || value.len() > 200
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
        {
            return false;
        }
        match key {
            "per_page" => value
                .parse::<u8>()
                .ok()
                .is_some_and(|value| (1..=100).contains(&value)),
            "page" => value
                .parse::<u32>()
                .ok()
                .is_some_and(|value| (1..=10000).contains(&value)),
            "state" | "status" | "branch" | "head_sha" | "sort" | "direction" | "filter" => true,
            _ => false,
        }
    })
}

pub(super) fn log_redirect(location: &str) -> Option<reqwest::Url> {
    if location.len() > 8192 {
        return None;
    }
    let url = reqwest::Url::parse(location).ok()?;
    let host = url.host_str()?;
    (url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && (host.ends_with(".blob.core.windows.net")
            || host.ends_with(".actions.githubusercontent.com")))
    .then_some(url)
}
