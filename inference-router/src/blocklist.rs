//! Domain blocklist engine with auto-refresh from threat intelligence feeds.
//!
//! The blocklist prevents sandboxed agents from reaching known-malicious domains
//! (malware C2, phishing, cryptojacking, etc.). It combines:
//!
//! - **Seed file**: loaded from ConfigMap at startup (always available, even offline)
//! - **Live feeds**: refreshed in background from OISD and URLhaus on a timer
//! - **High-risk TLDs**: blocked unconditionally (.tk, .ml, .ga, .cf, .gq — free TLDs
//!   abused by >80% of phishing campaigns per APWG data)
//! - **IP-direct blocking**: bare IP addresses in URLs are suspicious and blocked
//!
//! The blocklist is stored in an `Arc<RwLock<HashSet<String>>>` so reads (every proxy
//! request) are lock-free when uncontested, and writes (every 6h refresh) are rare.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;

/// High-risk TLDs frequently abused for phishing/malware.
/// Source: APWG Phishing Activity Trends, Spamhaus TLD stats.
const HIGH_RISK_TLDS: &[&str] = &[
    ".tk", ".ml", ".ga", ".cf", ".gq", // Freenom free TLDs
    ".top", ".buzz", ".surf", ".rest",  // Cheap bulk-registration TLDs
    ".onion", // Tor hidden services
];

/// How often to refresh from upstream feeds (default: 6 hours).
const DEFAULT_REFRESH_SECS: u64 = 6 * 3600;

/// Max domains to load per feed (prevents memory exhaustion from malformed feeds).
const MAX_DOMAINS_PER_FEED: usize = 500_000;

/// Well-known threat intelligence feed URLs.
const OISD_SMALL_URL: &str = "https://small.oisd.nl/domainswild";
const URLHAUS_URL: &str = "https://urlhaus.abuse.ch/downloads/hostfile/";

/// Thread-safe blocklist that can be shared across handlers and updated in background.
#[derive(Clone)]
pub struct Blocklist {
    domains: Arc<RwLock<HashSet<String>>>,
    high_risk_tlds_enabled: bool,
    ip_direct_blocked: bool,
    enabled: bool,
    /// Learn mode: log all accessed domains instead of relying on a static allowlist.
    /// Blocklist (known-bad) is STILL enforced in learn mode.
    learn_mode: Arc<AtomicBool>,
    /// Domains observed during learn mode (for generating an allowlist later).
    learned_domains: Arc<RwLock<HashSet<String>>>,
    /// Allowlist: explicitly approved domains for egress proxy.
    /// Only domains on this list can be fetched via /egress/fetch (unless learn mode is on).
    allowlist: Arc<RwLock<HashSet<String>>>,
    /// Pending approval requests: domains the agent tried to reach but aren't allowlisted.
    /// Capped at MAX_PENDING to prevent unbounded memory growth (#13).
    pending_approvals: Arc<RwLock<Vec<PendingApproval>>>,
}

/// Maximum pending approval entries to prevent unbounded memory growth.
const MAX_PENDING: usize = 1000;

/// A pending egress approval request.
#[derive(Clone, Debug, serde::Serialize)]
pub struct PendingApproval {
    pub id: String,
    pub domain: String,
    pub url: String,
    pub sandbox: String,
    pub timestamp: String,
    /// "pending" = normal allowlist request, "🛑 ech" = ECH block,
    /// "🛑 no-sni" = missing SNI, "🛑 dns-rebind" = private IP resolution,
    /// "🛑 ssrf" = SSRF attempt via egress fetch.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Human-readable explanation of why this was blocked.
    #[serde(default)]
    pub reason: String,
}

#[allow(dead_code)]
fn default_kind() -> String {
    "pending".into()
}

impl Blocklist {
    /// Create a new empty blocklist (disabled mode — passes everything).
    pub fn disabled() -> Self {
        Self {
            domains: Arc::new(RwLock::new(HashSet::new())),
            high_risk_tlds_enabled: false,
            ip_direct_blocked: false,
            enabled: false,
            learn_mode: Arc::new(AtomicBool::new(false)),
            learned_domains: Arc::new(RwLock::new(HashSet::new())),
            allowlist: Arc::new(RwLock::new(HashSet::new())),
            pending_approvals: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Create a new blocklist, loading the seed file if present.
    pub async fn new(seed_path: Option<&str>) -> Self {
        let mut domains = HashSet::new();

        // Load seed blocklist from ConfigMap-mounted file
        if let Some(path) = seed_path {
            if Path::new(path).exists() {
                match tokio::fs::read_to_string(path).await {
                    Ok(content) => {
                        let count = parse_domain_list(&content, &mut domains);
                        tracing::info!(count, path, "Loaded seed blocklist from file");
                    }
                    Err(e) => {
                        tracing::warn!(path, error = %e, "Failed to read seed blocklist");
                    }
                }
            } else {
                tracing::info!(path, "No seed blocklist file found — starting empty");
            }
        }

        Self {
            domains: Arc::new(RwLock::new(domains)),
            high_risk_tlds_enabled: true,
            ip_direct_blocked: true,
            enabled: true,
            learn_mode: Arc::new(AtomicBool::new(false)),
            learned_domains: Arc::new(RwLock::new(HashSet::new())),
            allowlist: Arc::new(RwLock::new(HashSet::new())),
            pending_approvals: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Check if a domain is blocked.
    /// Extracts domain from various input formats (URL, host:port, bare domain).
    pub async fn is_blocked(&self, input: &str) -> BlockResult {
        if !self.enabled {
            return BlockResult::Allowed;
        }

        let domain = extract_domain(input);

        // Block bare IP addresses (no DNS = suspicious)
        if self.ip_direct_blocked && is_ip_address(domain) {
            return BlockResult::Blocked {
                reason: "IP-direct access blocked — use DNS hostnames".into(),
                domain: domain.to_string(),
            };
        }

        // Check high-risk TLDs
        if self.high_risk_tlds_enabled {
            let lower = domain.to_lowercase();
            for tld in HIGH_RISK_TLDS {
                if lower.ends_with(tld) {
                    return BlockResult::Blocked {
                        reason: format!("High-risk TLD {tld} blocked"),
                        domain: domain.to_string(),
                    };
                }
            }
        }

        // Check domain + all parent domains against the blocklist
        let domains = self.domains.read().await;
        let lower = domain.to_lowercase();

        // Exact match
        if domains.contains(&lower) {
            return BlockResult::Blocked {
                reason: "Domain in threat intelligence blocklist".into(),
                domain: lower,
            };
        }

        // Subdomain match: check each parent (e.g., evil.example.com → example.com)
        let mut parts: &str = &lower;
        while let Some(pos) = parts.find('.') {
            parts = &parts[pos + 1..];
            if domains.contains(parts) {
                return BlockResult::Blocked {
                    reason: format!("Parent domain {parts} in threat intelligence blocklist"),
                    domain: lower,
                };
            }
        }

        BlockResult::Allowed
    }

    /// Start the background refresh task. Call once at startup.
    /// Fetches from OISD + URLhaus every `refresh_interval` seconds.
    pub fn start_refresh_task(
        &self,
        client: reqwest::Client,
        refresh_secs: Option<u64>,
        seed_path: Option<String>,
    ) {
        let domains = Arc::clone(&self.domains);
        let interval = Duration::from_secs(refresh_secs.unwrap_or(DEFAULT_REFRESH_SECS));

        tokio::spawn(async move {
            // Short initial delay, then retry quickly if feeds fail on first attempt.
            // Some containers have DNS not ready at startup — retry fixes that.
            tokio::time::sleep(Duration::from_secs(10)).await;

            // First fetch with aggressive retry (covers startup DNS timing)
            let mut first_run = true;

            loop {
                tracing::info!("Blocklist refresh: fetching upstream feeds");

                let mut new_domains = HashSet::new();

                // Re-load seed file (controller may have updated it via CronJob)
                if let Some(ref path) = seed_path
                    && let Ok(content) = tokio::fs::read_to_string(path).await
                {
                    let count = parse_domain_list(&content, &mut new_domains);
                    tracing::info!(count, "Blocklist: reloaded seed file");
                }

                // Fetch OISD with retry on first run
                let max_attempts = if first_run { 3 } else { 1 };
                let mut oisd_ok = false;
                for attempt in 1..=max_attempts {
                    match fetch_feed(&client, OISD_SMALL_URL).await {
                        Ok(content) => {
                            let count = parse_domain_list(&content, &mut new_domains);
                            tracing::info!(count, "Blocklist: loaded OISD feed");
                            oisd_ok = true;
                            break;
                        }
                        Err(e) => {
                            if attempt < max_attempts {
                                tracing::warn!(attempt, error = %e, "Blocklist: OISD fetch failed — retrying in 10s");
                                tokio::time::sleep(Duration::from_secs(10)).await;
                            } else {
                                tracing::warn!(error = %e, "Blocklist: OISD fetch failed — keeping existing");
                            }
                        }
                    }
                }

                // Fetch URLhaus (hostfile format: "127.0.0.1 domain")
                match fetch_feed(&client, URLHAUS_URL).await {
                    Ok(content) => {
                        let count = parse_hostfile(&content, &mut new_domains);
                        tracing::info!(count, "Blocklist: loaded URLhaus feed");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Blocklist: URLhaus fetch failed — keeping existing");
                    }
                }

                // Only replace if we actually got domains (don't wipe on network failure)
                if !new_domains.is_empty() {
                    let count = new_domains.len();
                    let mut lock = domains.write().await;
                    *lock = new_domains;
                    tracing::info!(count, "Blocklist refreshed — {count} domains blocked");
                } else {
                    tracing::warn!("Blocklist: all feeds failed, keeping previous entries");
                }

                // On first run, if OISD failed, retry sooner (60s instead of 6h)
                if first_run && !oisd_ok {
                    tracing::info!("Blocklist: OISD failed on startup — retrying in 60s");
                    tokio::time::sleep(Duration::from_secs(60)).await;
                } else {
                    tokio::time::sleep(interval).await;
                }
                first_run = false;
            }
        });
    }

    /// Returns current blocklist size (for health/metrics).
    pub async fn len(&self) -> usize {
        self.domains.read().await.len()
    }

    /// Returns true if the blocklist has no entries.
    pub async fn is_empty(&self) -> bool {
        self.domains.read().await.is_empty()
    }

    /// Enable or disable learn mode at runtime.
    pub fn set_learn_mode(&self, enabled: bool) {
        self.learn_mode.store(enabled, Ordering::Relaxed);
        if enabled {
            tracing::info!(
                "Egress learn mode enabled — logging all accessed domains (blocklist still enforced)"
            );
        } else {
            tracing::info!("Egress learn mode disabled");
        }
    }

    /// Returns whether learn mode is active.
    pub fn is_learn_mode(&self) -> bool {
        self.learn_mode.load(Ordering::Relaxed)
    }

    /// Record a domain as observed during learn mode.
    pub async fn record_learned(&self, domain: &str) {
        if self.is_learn_mode() {
            let domain = extract_domain(domain).to_lowercase();
            if !domain.is_empty() && is_valid_domain(&domain) {
                self.learned_domains.write().await.insert(domain);
            }
        }
    }

    /// Get all learned domains (for generating an allowlist).
    pub async fn get_learned_domains(&self) -> Vec<String> {
        let mut domains: Vec<String> = self.learned_domains.read().await.iter().cloned().collect();
        domains.sort();
        domains
    }

    /// Get learned domain count.
    pub async fn learned_count(&self) -> usize {
        self.learned_domains.read().await.len()
    }

    /// Clear learned domains (after export/review).
    pub async fn clear_learned(&self) {
        self.learned_domains.write().await.clear();
    }

    // ── Allowlist (for egress proxy) ──

    /// Check egress access: blocklist → allowlist → pending approval.
    /// Returns Ok(()) if allowed, Err(reason) if denied.
    pub async fn check_egress(&self, url: &str, sandbox: &str) -> Result<(), String> {
        // 1. Blocklist: hard deny
        let block_result = self.is_blocked(url).await;
        if block_result.is_blocked() {
            return Err("Domain blocked by threat intelligence blocklist".into());
        }

        let domain = extract_domain(url).to_lowercase();

        // 2. Learn mode: allow + record (for discovery)
        if self.is_learn_mode() {
            self.record_learned(url).await;
            return Ok(());
        }

        // 3. Allowlist: explicitly approved domains pass through
        {
            let al = self.allowlist.read().await;
            if al.contains(&domain) {
                return Ok(());
            }
            // Check parent domains (e.g., allowlisting "telegram.org" covers "api.telegram.org")
            let mut parts: &str = &domain;
            while let Some(pos) = parts.find('.') {
                parts = &parts[pos + 1..];
                if al.contains(parts) {
                    return Ok(());
                }
            }
        }

        // 4. Not allowlisted → create pending approval (dedup by domain, capped)
        {
            let mut pending = self.pending_approvals.write().await;
            let already_pending = pending.iter().any(|p| p.domain == domain);
            if !already_pending && pending.len() < MAX_PENDING {
                let id = format!("{:x}", md5_hash(&format!("{}{}", domain, sandbox)));
                pending.push(PendingApproval {
                    id,
                    domain: domain.clone(),
                    url: url.to_string(),
                    sandbox: sandbox.to_string(),
                    timestamp: format!(
                        "{}Z",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                    ),
                    kind: "pending".into(),
                    reason: format!(
                        "Domain '{}' not on allowlist — awaiting operator approval",
                        domain
                    ),
                });
            }
        }

        Err(format!(
            "Domain '{}' not on allowlist — pending operator approval",
            domain
        ))
    }

    /// Add a domain to the allowlist.
    pub async fn allow_domain(&self, domain: &str) {
        let domain = domain.to_lowercase();
        self.allowlist.write().await.insert(domain.clone());
        // Remove from pending approvals
        self.pending_approvals
            .write()
            .await
            .retain(|p| p.domain != domain);
        tracing::info!(domain = %domain, "Domain added to egress allowlist");
    }

    /// Remove a domain from the allowlist.
    pub async fn deny_domain(&self, domain: &str) {
        let domain = domain.to_lowercase();
        self.allowlist.write().await.remove(&domain);
        self.pending_approvals
            .write()
            .await
            .retain(|p| p.domain != domain);
    }

    /// Get the current allowlist.
    pub async fn get_allowlist(&self) -> Vec<String> {
        let mut domains: Vec<String> = self.allowlist.read().await.iter().cloned().collect();
        domains.sort();
        domains
    }

    /// Get pending approval requests.
    pub async fn get_pending_approvals(&self) -> Vec<PendingApproval> {
        self.pending_approvals.read().await.clone()
    }

    /// Record a proxy-level block (ECH, missing SNI, private IP resolution).
    /// Surfaces in the pending approvals queue so operators can see what was blocked
    /// and why, rather than silently dropping connections.
    pub async fn record_proxy_block(&self, domain: &str, kind: &str, reason: &str, sandbox: &str) {
        let mut pending = self.pending_approvals.write().await;
        // Dedup by domain + kind
        let already = pending.iter().any(|p| p.domain == domain && p.kind == kind);
        if !already && pending.len() < MAX_PENDING {
            let id = format!("{:x}", md5_hash(&format!("proxy:{}:{}", domain, kind)));
            pending.push(PendingApproval {
                id,
                domain: domain.to_string(),
                url: String::new(),
                sandbox: sandbox.to_string(),
                timestamp: format!(
                    "{}Z",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                ),
                kind: kind.to_string(),
                reason: reason.to_string(),
            });
        }
    }
}

/// Simple hash for generating approval IDs.
fn md5_hash(input: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    hasher.finish()
}

/// Result of a blocklist check.
#[derive(Debug)]
pub enum BlockResult {
    Allowed,
    Blocked { reason: String, domain: String },
}

impl BlockResult {
    #[allow(dead_code)]
    pub fn is_blocked(&self) -> bool {
        matches!(self, BlockResult::Blocked { .. })
    }
}

/// Parse a domain list (one domain per line, # comments, wildcards stripped).
fn parse_domain_list(content: &str, dest: &mut HashSet<String>) -> usize {
    let mut count = 0;
    for line in content.lines() {
        if count >= MAX_DOMAINS_PER_FEED {
            break;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        // Strip wildcard prefix (*.example.com → example.com)
        let domain = line.trim_start_matches("*.");
        let domain = domain.to_lowercase();
        if is_valid_domain(&domain) {
            dest.insert(domain);
            count += 1;
        }
    }
    count
}

/// Parse hostfile format ("127.0.0.1 domain" or "0.0.0.0 domain").
fn parse_hostfile(content: &str, dest: &mut HashSet<String>) -> usize {
    let mut count = 0;
    for line in content.lines() {
        if count >= MAX_DOMAINS_PER_FEED {
            break;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Format: "127.0.0.1\tdomain" or "0.0.0.0 domain"
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && (parts[0] == "127.0.0.1" || parts[0] == "0.0.0.0") {
            let domain = parts[1].to_lowercase();
            if is_valid_domain(&domain) {
                dest.insert(domain);
                count += 1;
            }
        }
    }
    count
}

/// Fetch a feed URL with timeout and size limits.
async fn fetch_feed(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(60))
        .header("user-agent", "AzureClaw-Blocklist/1.0")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }

    // Limit to 50MB to prevent memory exhaustion
    let bytes = resp.bytes().await?;
    if bytes.len() > 50 * 1024 * 1024 {
        anyhow::bail!("Feed exceeds 50MB limit");
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Extract domain from a URL, host:port, or bare domain.
fn extract_domain(input: &str) -> &str {
    let s = input.trim();
    // Strip scheme
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    // Take host part (before / or ?)
    let s = s.split('/').next().unwrap_or(s);
    let s = s.split('?').next().unwrap_or(s);
    // Strip port
    if let Some(bracket_end) = s.find(']') {
        // IPv6 [::1]:port
        &s[..bracket_end + 1]
    } else if let Some(colon) = s.rfind(':') {
        // Check if everything after colon is digits (port)
        if s[colon + 1..].chars().all(|c| c.is_ascii_digit()) {
            &s[..colon]
        } else {
            s
        }
    } else {
        s
    }
}

/// Basic domain validation (not empty, has at least one dot, no spaces).
fn is_valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.contains('.')
        && !domain.contains(' ')
        && domain.len() < 256
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// Check if a string looks like an IP address (v4 or v6).
fn is_ip_address(s: &str) -> bool {
    // IPv4: all digits and dots
    if s.chars().all(|c| c.is_ascii_digit() || c == '.') && s.contains('.') {
        return s.parse::<std::net::Ipv4Addr>().is_ok();
    }
    // IPv6: contains colons
    if s.contains(':') {
        let s = s.trim_start_matches('[').trim_end_matches(']');
        return s.parse::<std::net::Ipv6Addr>().is_ok();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_domain() {
        assert_eq!(extract_domain("https://evil.com/path"), "evil.com");
        assert_eq!(extract_domain("http://malware.tk:8080/c2"), "malware.tk");
        assert_eq!(extract_domain("evil.com"), "evil.com");
        assert_eq!(extract_domain("sub.evil.com:443"), "sub.evil.com");
    }

    #[test]
    fn test_parse_domain_list() {
        let input = "# comment\n*.evil.com\ngood.com\n!adblock-rule\nbad.tk\n";
        let mut set = HashSet::new();
        let count = parse_domain_list(input, &mut set);
        assert_eq!(count, 3);
        assert!(set.contains("evil.com"));
        assert!(set.contains("good.com"));
        assert!(set.contains("bad.tk"));
    }

    #[test]
    fn test_parse_hostfile() {
        let input = "# URLhaus\n127.0.0.1\tmalware.com\n0.0.0.0 phish.net\n";
        let mut set = HashSet::new();
        let count = parse_hostfile(input, &mut set);
        assert_eq!(count, 2);
        assert!(set.contains("malware.com"));
        assert!(set.contains("phish.net"));
    }

    #[test]
    fn test_is_ip_address() {
        assert!(is_ip_address("192.168.1.1"));
        assert!(is_ip_address("10.0.0.1"));
        assert!(!is_ip_address("evil.com"));
        assert!(!is_ip_address("sub.domain.com"));
    }

    #[tokio::test]
    async fn test_high_risk_tld_blocking() {
        let bl = Blocklist::new(None).await;
        assert!(bl.is_blocked("evil.tk").await.is_blocked());
        assert!(bl.is_blocked("phish.ml").await.is_blocked());
        assert!(
            bl.is_blocked("https://malware.ga/payload")
                .await
                .is_blocked()
        );
        assert!(!bl.is_blocked("microsoft.com").await.is_blocked());
    }

    #[tokio::test]
    async fn test_ip_direct_blocking() {
        let bl = Blocklist::new(None).await;
        assert!(bl.is_blocked("192.168.1.1").await.is_blocked());
        assert!(bl.is_blocked("http://10.0.0.1/steal").await.is_blocked());
        assert!(!bl.is_blocked("api.microsoft.com").await.is_blocked());
    }

    #[tokio::test]
    async fn test_disabled_blocklist() {
        let bl = Blocklist::disabled();
        assert!(!bl.is_blocked("evil.tk").await.is_blocked());
        assert!(!bl.is_blocked("192.168.1.1").await.is_blocked());
    }
}
