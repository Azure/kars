// kars Bridge BFF — network pre-flight checks.

use std::time::Duration;

use super::{Check, CheckStatus};

/// Extract the host from a URL string for a reachability check. Best-effort:
/// strips a scheme and any path/port. Returns `None` for an empty host.
pub(super) fn url_host(url: &str) -> Option<String> {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host = after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// True when `host:port` resolves in DNS within a short timeout. A real,
/// honest reachability signal from the gateway — not a full connection.
pub(super) async fn resolves(host: &str, port: u16) -> bool {
    let addr = format!("{host}:{port}");
    tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(&addr))
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

pub(super) async fn check_egress(bp: &crate::routes::tasks::BlueprintDto, checks: &mut Vec<Check>) {
    // 5. Egress hosts resolve (DNS) — a real, honest reachability signal from
    //    the BFF (not the full in-sandbox egress path, which is a deeper probe).
    for e in &bp.egress {
        let port = e.port.unwrap_or(443) as u16;
        let resolved = resolves(&e.host, port).await;
        checks.push(Check {
            id: format!("egress:{}", e.host),
            label: if resolved {
                format!("Egress host {} resolves", e.host)
            } else {
                format!("Egress host {} does not resolve", e.host)
            },
            status: if resolved { CheckStatus::Pass } else { CheckStatus::Warn },
            detail: if resolved {
                "The host resolves in DNS. Full reachability from the sandbox egress path is a deeper probe (named next step).".into()
            } else {
                "The host did not resolve from the gateway. Check the spelling; it may still be reachable from inside the cluster.".into()
            },
        });
    }
}
