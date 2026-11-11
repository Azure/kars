// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Domain blocklist engine with auto-refresh from threat intelligence feeds.
//!
//! The blocklist prevents sandboxed agents from reaching known-malicious domains
//! (malware C2, phishing, cryptojacking, etc.). It combines:
//!
//! - **Seed file**: loaded from ConfigMap at startup (always available, even offline)
//! - **Live feeds**: refreshed in background from OISD on a timer, plus any opt-in
//!   feeds named in `BLOCKLIST_EXTRA_FEEDS` (empty by default)
//! - **High-risk TLDs**: blocked unconditionally (.tk, .ml, .ga, .cf, .gq — free TLDs
//!   abused by >80% of phishing campaigns per APWG data)
//! - **IP-direct blocking**: bare IP addresses in URLs are suspicious and blocked
//!
//! The blocklist is stored in an `Arc<RwLock<HashSet<String>>>` so reads (every proxy
//! request) are lock-free when uncontested, and writes (every 6h refresh) are rare.
//!
//! Every source (seed file, OISD, each extra feed) keeps the entries of its last
//! successful load. A source that fails in one cycle keeps contributing those
//! entries, so a partial outage never drops what that source supplied before.
//! Limits: 50 MB per feed body (enforced while streaming), 500,000 domains per
//! source and [`MAX_TOTAL_DOMAINS`] across all sources.

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

/// Max domains kept from one source: the seed file, OISD or one extra feed
/// (prevents memory exhaustion from malformed feeds).
const MAX_DOMAINS_PER_FEED: usize = 500_000;

/// Max domains kept from all sources together. A source only gets the space
/// the other sources leave, and the refresh logs the truncation. Each source
/// keeps its last-known-good set, so the router holds at most this many
/// entries in the per-source sets plus as many again in the merged set.
pub const MAX_TOTAL_DOMAINS: usize = 1_000_000;

/// Max body size of one feed after decompression. Enforced while the body
/// is streamed, so an oversized feed is never buffered whole.
const MAX_FEED_BYTES: usize = 50 * 1024 * 1024;

/// Timeout of one feed download, from connect to the last body byte.
const FEED_TIMEOUT: Duration = Duration::from_secs(60);

/// Well-known threat intelligence feed URL.
const OISD_SMALL_URL: &str = "https://small.oisd.nl/domainswild";

/// Max opt-in feeds accepted from `BLOCKLIST_EXTRA_FEEDS`.
pub const MAX_EXTRA_FEEDS: usize = 8;

/// Line format of an opt-in feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedFormat {
    /// Hostfile lines: `0.0.0.0 domain` or `127.0.0.1 domain`.
    Hosts,
    /// One domain per line (`*.` prefix stripped), like OISD.
    Domains,
}

/// An opt-in feed from `BLOCKLIST_EXTRA_FEEDS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraFeed {
    pub format: FeedFormat,
    pub url: String,
    /// Host of `url`, used in logs instead of the full URL.
    pub host: String,
}

/// Parse `BLOCKLIST_EXTRA_FEEDS`: comma-separated `hosts:<https-url>` /
/// `domains:<https-url>` entries. Returns the accepted feeds and one
/// message per ignored entry. Only `https` URLs without credentials are
/// accepted, duplicate URLs are dropped and at most [`MAX_EXTRA_FEEDS`]
/// feeds are kept. Messages name the entry by position, never by URL.
pub fn parse_extra_feeds(value: &str) -> (Vec<ExtraFeed>, Vec<String>) {
    let mut feeds: Vec<ExtraFeed> = Vec::new();
    let mut ignored = Vec::new();
    let entries = value.split(',').map(str::trim).filter(|e| !e.is_empty());
    for (i, entry) in entries.enumerate() {
        let n = i + 1;
        let Some((kind, url)) = entry.split_once(':') else {
            ignored.push(format!(
                "entry {n}: expected hosts:<https-url> or domains:<https-url>"
            ));
            continue;
        };
        let format = match kind.trim().to_ascii_lowercase().as_str() {
            "hosts" => FeedFormat::Hosts,
            "domains" => FeedFormat::Domains,
            _ => {
                ignored.push(format!("entry {n}: unknown format, use hosts: or domains:"));
                continue;
            }
        };
        let url = url.trim();
        let Some(host) = https_feed_host(url) else {
            ignored.push(format!(
                "entry {n}: only https URLs with a host and without credentials are accepted"
            ));
            continue;
        };
        if feeds.iter().any(|f| f.url == url) {
            ignored.push(format!("entry {n}: duplicate URL"));
            continue;
        }
        if feeds.len() >= MAX_EXTRA_FEEDS {
            ignored.push(format!(
                "entry {n}: more than {MAX_EXTRA_FEEDS} extra feeds"
            ));
            continue;
        }
        feeds.push(ExtraFeed {
            format,
            url: url.to_string(),
            host,
        });
    }
    (feeds, ignored)
}

/// Host of an https URL without credentials, or `None`.
fn https_feed_host(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() {
        return None;
    }
    u.host_str().map(str::to_string)
}

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
    /// Egress allowlist (L7 hostname filter). Populated by the
    /// `egress_allowlist_loader` from the signed bundle compiled by
    /// the controller (Slice 5c.1) — the in-memory state is **fully
    /// derived from the mount** and is replaced atomically on every
    /// hot-reload via [`Blocklist::replace_allowlist`]. There is no
    /// HTTP endpoint that can mutate this set; the in-process
    /// `POST /egress/approve` admin path (and its `PendingApproval`
    /// surface) was removed alongside this slice — see
    /// `docs/internal/crd-well-oiled-machine/slice-5-egress-polish-and-observability.md`.
    allowlist: Arc<RwLock<HashSet<String>>>,
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
                        let count = parse_domain_list(&content, &mut domains, MAX_DOMAINS_PER_FEED);
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
    /// Fetches from OISD and the opt-in `extra_feeds` every `refresh_interval` seconds.
    pub fn start_refresh_task(
        &self,
        client: reqwest::Client,
        refresh_secs: Option<u64>,
        seed_path: Option<String>,
        extra_feeds: Vec<ExtraFeed>,
    ) {
        let domains = Arc::clone(&self.domains);
        let interval = Duration::from_secs(refresh_secs.unwrap_or(DEFAULT_REFRESH_SECS));
        let mut sources = RefreshSources::new(seed_path, &extra_feeds, FeedLimits::default());

        tokio::spawn(async move {
            // Short initial delay, then retry quickly if feeds fail on first attempt.
            // Some containers have DNS not ready at startup — retry fixes that.
            tokio::time::sleep(Duration::from_secs(10)).await;

            // First fetch with aggressive retry (covers startup DNS timing)
            let mut first_run = true;

            loop {
                tracing::info!("Blocklist refresh: fetching upstream feeds");

                let outcome = sources.refresh(&client, first_run).await;

                // Only replace if we have domains (don't wipe on network failure).
                // Failed sources still contribute their last-known-good entries.
                if !outcome.merged.is_empty() {
                    let count = outcome.merged.len();
                    let mut lock = domains.write().await;
                    *lock = outcome.merged;
                    tracing::info!(
                        count,
                        failed_sources = outcome.failed,
                        "Blocklist refreshed — {count} domains blocked"
                    );
                } else {
                    tracing::warn!("Blocklist: no source has loaded yet, keeping previous entries");
                }

                // On first run, if OISD failed, retry sooner (60s instead of 6h)
                if first_run && !outcome.primary_ok {
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

    /// Check egress access: blocklist → learn-mode bypass → allowlist
    /// (with parent-domain match). Returns `Ok(())` if allowed,
    /// `Err(reason)` if denied.
    ///
    /// Slice 5c.1 removed the in-process "pending approval" side
    /// effect that used to fire here. The allowlist is now fully
    /// derived from the controller-published bundle mounted at
    /// `${EGRESS_ALLOWLIST_DIR}/allowlist.json` — operator-driven
    /// approvals will land in Slice 5c.2 as a separate
    /// `EgressApproval` CRD with its own mount; the runtime path
    /// merges `bundle ∪ approvals` here, not via a privileged HTTP
    /// endpoint. The `sandbox` arg is preserved so callers (the
    /// `BlockedBuffer` observability surface in `forward_proxy`) can
    /// continue to attribute denials.
    pub async fn check_egress(&self, url: &str, _sandbox: &str) -> Result<(), String> {
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

        // 4. Not allowlisted → deny. Observability is recorded by the
        //    forward-proxy caller into the slice-5a `BlockedBuffer`
        //    (`GET /egress/blocked`); there is no longer an
        //    in-process "approve this domain" surface.
        Err(format!(
            "Domain '{}' not on signed allowlist — operator must apply an EgressApproval (Slice 5c.2) or update the bundle",
            domain
        ))
    }

    /// Atomically replace the egress allowlist with `domains`. Called
    /// by `egress_allowlist_loader` on startup and on every hot-reload
    /// — the loader holds the authoritative bundle digest, so the
    /// router-side view is always exactly what the controller
    /// published. `domains` are lower-cased before insertion to
    /// match the lookup path in [`Blocklist::check_egress`].
    ///
    /// The replacement is performed under a single write-lock so
    /// concurrent readers never observe a partial allowlist (mid-add
    /// or mid-remove).
    pub async fn replace_allowlist(&self, domains: Vec<String>) {
        let normalized: HashSet<String> = domains
            .into_iter()
            .map(|d| d.trim().to_ascii_lowercase())
            .filter(|d| !d.is_empty())
            .collect();
        let count = normalized.len();
        let mut al = self.allowlist.write().await;
        *al = normalized;
        tracing::info!(count, "Egress allowlist replaced from signed bundle");
    }

    /// Get the current allowlist.
    pub async fn get_allowlist(&self) -> Vec<String> {
        let mut domains: Vec<String> = self.allowlist.read().await.iter().cloned().collect();
        domains.sort();
        domains
    }
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
/// Stops once `dest` holds `limit` entries.
fn parse_domain_list(content: &str, dest: &mut HashSet<String>, limit: usize) -> usize {
    let mut count = 0;
    for line in content.lines() {
        if dest.len() >= limit {
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

/// Parse feed content in the given line format. Stops once `dest` holds
/// `limit` entries.
fn parse_feed(
    format: FeedFormat,
    content: &str,
    dest: &mut HashSet<String>,
    limit: usize,
) -> usize {
    match format {
        FeedFormat::Hosts => parse_hostfile(content, dest, limit),
        FeedFormat::Domains => parse_domain_list(content, dest, limit),
    }
}

/// Parse hostfile format ("127.0.0.1 domain" or "0.0.0.0 domain").
/// Stops once `dest` holds `limit` entries.
fn parse_hostfile(content: &str, dest: &mut HashSet<String>, limit: usize) -> usize {
    let mut count = 0;
    for line in content.lines() {
        if dest.len() >= limit {
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

/// Why a feed could not be loaded. `Display` never includes the feed URL,
/// which may carry a token in its query string: logs name a feed by host.
#[derive(Debug)]
enum FeedError {
    /// Transport, timeout or body error, with the URL stripped.
    Request(reqwest::Error),
    /// The feed answered with a redirect. Redirects are never followed:
    /// a feed must return its content directly from the configured URL.
    Redirect(reqwest::StatusCode),
    /// Any other non-success status.
    Status(reqwest::StatusCode),
    /// The body passed the byte cap (after decompression).
    TooLarge(usize),
    /// The seed file could not be read.
    Seed(std::io::Error),
}

impl FeedError {
    fn request(e: reqwest::Error) -> Self {
        Self::Request(e.without_url())
    }
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(e) => write!(f, "{e}"),
            Self::Redirect(status) => write!(
                f,
                "HTTP {status}: redirects are not followed, configure the URL that serves the list"
            ),
            Self::Status(status) => write!(f, "HTTP {status}"),
            Self::TooLarge(max) => write!(f, "feed exceeds the {max}-byte limit"),
            Self::Seed(e) => write!(f, "{e}"),
        }
    }
}

/// Fetch a feed with a timeout and a byte cap. The body is streamed and the
/// cap is checked chunk by chunk, so an oversized feed is dropped without
/// being buffered whole. A redirect is an error (the shared client does not
/// follow redirects, and a feed that moves must be reconfigured).
async fn fetch_feed(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
) -> Result<String, FeedError> {
    let mut resp = client
        .get(url)
        .timeout(FEED_TIMEOUT)
        .header("user-agent", "kars-Blocklist/1.0")
        .send()
        .await
        .map_err(FeedError::request)?;

    let status = resp.status();
    // A client that followed redirects would end on another URL: refuse that
    // too, so the rule does not depend on how the shared client is built.
    let moved = reqwest::Url::parse(url).map_or(true, |u| u != *resp.url());
    if status.is_redirection() || moved {
        return Err(FeedError::Redirect(status));
    }
    if !status.is_success() {
        return Err(FeedError::Status(status));
    }
    if let Some(len) = resp.content_length()
        && len > max_bytes as u64
    {
        return Err(FeedError::TooLarge(max_bytes));
    }

    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(FeedError::request)? {
        if body.len() + chunk.len() > max_bytes {
            return Err(FeedError::TooLarge(max_bytes));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Size limits of the refresh loop. Tests use small values.
#[derive(Debug, Clone, Copy)]
struct FeedLimits {
    /// Max domains kept from one source.
    per_source: usize,
    /// Max domains kept from all sources together.
    total: usize,
    /// Max body size of one feed.
    max_bytes: usize,
}

impl Default for FeedLimits {
    fn default() -> Self {
        Self {
            per_source: MAX_DOMAINS_PER_FEED,
            total: MAX_TOTAL_DOMAINS,
            max_bytes: MAX_FEED_BYTES,
        }
    }
}

/// Where a source comes from.
enum SourceKind {
    /// The seed file (domain list), re-read every cycle.
    Seed(String),
    /// A feed URL. `primary` marks OISD: retried on the first run, and its
    /// failure triggers the quick startup retry.
    Feed {
        url: String,
        format: FeedFormat,
        primary: bool,
    },
}

/// One source of the refresh loop with its last-known-good entries.
struct Source {
    /// Name for logs ("seed", "OISD" or the feed host), never the URL.
    name: String,
    kind: SourceKind,
    /// Entries of the last successful load, kept while later loads fail.
    last_good: Option<HashSet<String>>,
}

impl Source {
    fn len(&self) -> usize {
        self.last_good.as_ref().map_or(0, HashSet::len)
    }
}

/// Result of one refresh cycle.
struct RefreshOutcome {
    /// Union of every source's current or last-known-good entries.
    merged: HashSet<String>,
    /// Number of sources that failed this cycle.
    failed: usize,
    /// Whether every primary source (OISD) loaded this cycle.
    primary_ok: bool,
}

/// All sources of the refresh loop: the seed file, OISD, then the extra
/// feeds in their configured order.
struct RefreshSources {
    sources: Vec<Source>,
    limits: FeedLimits,
}

impl RefreshSources {
    fn new(seed_path: Option<String>, extra_feeds: &[ExtraFeed], limits: FeedLimits) -> Self {
        let mut sources = Vec::new();
        if let Some(path) = seed_path {
            sources.push(Source {
                name: "seed".into(),
                kind: SourceKind::Seed(path),
                last_good: None,
            });
        }
        sources.push(Source {
            name: "OISD".into(),
            kind: SourceKind::Feed {
                url: OISD_SMALL_URL.into(),
                format: FeedFormat::Domains,
                primary: true,
            },
            last_good: None,
        });
        Self::with_sources(sources, extra_feeds, limits)
    }

    fn with_sources(
        mut sources: Vec<Source>,
        extra_feeds: &[ExtraFeed],
        limits: FeedLimits,
    ) -> Self {
        for feed in extra_feeds {
            sources.push(Source {
                name: feed.host.clone(),
                kind: SourceKind::Feed {
                    url: feed.url.clone(),
                    format: feed.format,
                    primary: false,
                },
                last_good: None,
            });
        }
        Self { sources, limits }
    }

    /// Load every source once. A source that loads replaces its entries;
    /// one that fails keeps the entries of its last successful load.
    async fn refresh(&mut self, client: &reqwest::Client, first_run: bool) -> RefreshOutcome {
        let mut failed = 0;
        let mut primary_ok = true;

        for i in 0..self.sources.len() {
            // A source gets the per-source cap, or what the others leave of
            // the total budget, whichever is smaller.
            let others: usize = self
                .sources
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, s)| s.len())
                .sum();
            let limit = self
                .limits
                .per_source
                .min(self.limits.total.saturating_sub(others));
            let limits = self.limits;
            let source = &mut self.sources[i];
            let is_primary = matches!(source.kind, SourceKind::Feed { primary: true, .. });

            match load_source(
                client,
                &source.kind,
                &source.name,
                first_run,
                limits.max_bytes,
            )
            .await
            {
                Ok((format, content)) => {
                    let mut set = HashSet::new();
                    parse_feed(format, &content, &mut set, limit);
                    let count = set.len();
                    if count >= limit {
                        if limit < limits.per_source {
                            tracing::warn!(
                                source = %source.name,
                                limit,
                                total = limits.total,
                                "Blocklist: aggregate domain budget reached, source truncated"
                            );
                        } else {
                            tracing::warn!(
                                source = %source.name,
                                limit,
                                "Blocklist: per-source domain cap reached, source truncated"
                            );
                        }
                    }
                    tracing::info!(count, source = %source.name, "Blocklist: source loaded");
                    source.last_good = Some(set);
                }
                // No seed file (e.g. overlay mode): nothing to load, not a failure.
                Err(FeedError::Seed(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    tracing::debug!(source = %source.name, "Blocklist: no seed file, skipped");
                }
                Err(e) => {
                    failed += 1;
                    if is_primary {
                        primary_ok = false;
                    }
                    match &source.last_good {
                        Some(set) => tracing::warn!(
                            source = %source.name,
                            error = %e,
                            kept = set.len(),
                            "Blocklist: source failed, keeping the entries of its last successful load"
                        ),
                        None => tracing::warn!(
                            source = %source.name,
                            error = %e,
                            "Blocklist: source failed and has never loaded, it adds no entries until it does"
                        ),
                    }
                }
            }
        }

        let total: usize = self.sources.iter().map(Source::len).sum();
        let mut merged = HashSet::with_capacity(total);
        for source in &self.sources {
            if let Some(set) = &source.last_good {
                merged.extend(set.iter().cloned());
            }
        }
        RefreshOutcome {
            merged,
            failed,
            primary_ok,
        }
    }
}

/// Read one source. OISD gets 3 attempts on the first run (startup DNS).
async fn load_source(
    client: &reqwest::Client,
    kind: &SourceKind,
    name: &str,
    first_run: bool,
    max_bytes: usize,
) -> Result<(FeedFormat, String), FeedError> {
    match kind {
        SourceKind::Seed(path) => tokio::fs::read_to_string(path)
            .await
            .map(|content| (FeedFormat::Domains, content))
            .map_err(FeedError::Seed),
        SourceKind::Feed {
            url,
            format,
            primary,
        } => {
            let max_attempts = if first_run && *primary { 3 } else { 1 };
            let mut attempt = 1;
            loop {
                match fetch_feed(client, url, max_bytes).await {
                    Ok(content) => return Ok((*format, content)),
                    Err(e) if attempt < max_attempts => {
                        tracing::warn!(source = %name, attempt, error = %e, "Blocklist: fetch failed — retrying in 10s");
                        tokio::time::sleep(Duration::from_secs(10)).await;
                        attempt += 1;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }
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
        let count = parse_domain_list(input, &mut set, MAX_DOMAINS_PER_FEED);
        assert_eq!(count, 3);
        assert!(set.contains("evil.com"));
        assert!(set.contains("good.com"));
        assert!(set.contains("bad.tk"));
    }

    #[test]
    fn test_parse_hostfile() {
        let input = "# URLhaus\n127.0.0.1\tmalware.com\n0.0.0.0 phish.net\n";
        let mut set = HashSet::new();
        let count = parse_hostfile(input, &mut set, MAX_DOMAINS_PER_FEED);
        assert_eq!(count, 2);
        assert!(set.contains("malware.com"));
        assert!(set.contains("phish.net"));
    }

    #[test]
    fn test_parse_extra_feeds_unset_is_empty() {
        for unset in ["", " , "] {
            let (feeds, ignored) = parse_extra_feeds(unset);
            assert!(feeds.is_empty());
            assert!(ignored.is_empty());
        }
    }

    #[test]
    fn test_parse_extra_feeds_accepts_hosts_and_domains() {
        let (feeds, ignored) = parse_extra_feeds(
            " hosts:https://feeds.example.com/list?format=hosts , DOMAINS:https://lists.example.org/domains.txt",
        );
        assert!(ignored.is_empty(), "{ignored:?}");
        assert_eq!(
            feeds,
            vec![
                ExtraFeed {
                    format: FeedFormat::Hosts,
                    url: "https://feeds.example.com/list?format=hosts".into(),
                    host: "feeds.example.com".into(),
                },
                ExtraFeed {
                    format: FeedFormat::Domains,
                    url: "https://lists.example.org/domains.txt".into(),
                    host: "lists.example.org".into(),
                },
            ]
        );
    }

    #[test]
    fn test_parse_extra_feeds_rejects_bad_entries() {
        let (feeds, ignored) = parse_extra_feeds(
            "hosts:http://plain.example.com/hosts,\
             https://no-format.example.com/list,\
             csv:https://unknown.example.com/list,\
             domains:https://user:secret@creds.example.com/list,\
             domains:not a url",
        );
        assert!(feeds.is_empty());
        assert_eq!(ignored.len(), 5, "{ignored:?}");
        assert!(ignored.iter().all(|m| !m.contains("secret")));
    }

    #[test]
    fn test_parse_extra_feeds_drops_duplicates_and_caps_count() {
        let list: Vec<String> = (0..MAX_EXTRA_FEEDS + 2)
            .map(|i| format!("domains:https://feed{i}.example.com/list"))
            .collect();
        let value = format!("{},domains:https://feed0.example.com/list", list.join(","));
        let (feeds, ignored) = parse_extra_feeds(&value);
        assert_eq!(feeds.len(), MAX_EXTRA_FEEDS);
        assert_eq!(ignored.len(), 3, "{ignored:?}");
    }

    #[test]
    fn test_parse_feed_by_format() {
        let mut set = HashSet::new();
        assert_eq!(
            parse_feed(FeedFormat::Hosts, "0.0.0.0 evil.example\n", &mut set, 10),
            1
        );
        assert_eq!(
            parse_feed(FeedFormat::Domains, "*.bad.example\n", &mut set, 10),
            1
        );
        assert!(set.contains("evil.example") && set.contains("bad.example"));
        // A plain domain line is not a hostfile line.
        assert_eq!(
            parse_feed(FeedFormat::Hosts, "other.example\n", &mut set, 10),
            0
        );
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

    /// Client like the router's: never follows redirects.
    fn test_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client")
    }

    fn feed(format: FeedFormat, url: String) -> ExtraFeed {
        let host = reqwest::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        ExtraFeed { format, url, host }
    }

    /// Sources made of the given feeds only (no seed, no OISD).
    fn feeds_only(feeds: &[ExtraFeed], limits: FeedLimits) -> RefreshSources {
        RefreshSources::with_sources(Vec::new(), feeds, limits)
    }

    #[tokio::test]
    async fn test_fetch_error_does_not_log_secret_query() {
        // A port with nothing listening: the request fails in transport, the
        // error type that carries the URL.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let url = format!("http://127.0.0.1:{port}/list?token=s3cr3t-value");

        let err = fetch_feed(&test_client(), &url, 1024).await.unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, FeedError::Request(_)), "{msg}");
        assert!(!msg.contains("s3cr3t-value"), "{msg}");
        assert!(!msg.contains("token="), "{msg}");
        assert!(!format!("{err:?}").contains("s3cr3t-value"));

        // HTTP error statuses carry no URL either.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let url = format!("{}/list?token=s3cr3t-value", server.uri());
        let err = fetch_feed(&test_client(), &url, 1024).await.unwrap_err();
        assert_eq!(err.to_string(), "HTTP 503 Service Unavailable");
    }

    #[tokio::test]
    async fn test_fetch_refuses_redirects() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/moved"))
            .respond_with(
                wiremock::ResponseTemplate::new(301)
                    .insert_header("location", "https://elsewhere.example/list"),
            )
            .mount(&server)
            .await;
        let err = fetch_feed(&test_client(), &format!("{}/moved", server.uri()), 1024)
            .await
            .unwrap_err();
        assert!(matches!(err, FeedError::Redirect(_)), "{err}");
        assert!(err.to_string().contains("redirects are not followed"));
    }

    #[tokio::test]
    async fn test_fetch_caps_declared_length() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/big"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("x".repeat(2048)))
            .mount(&server)
            .await;
        let url = format!("{}/big", server.uri());
        let err = fetch_feed(&test_client(), &url, 1024).await.unwrap_err();
        assert!(matches!(err, FeedError::TooLarge(1024)), "{err}");
        // The same feed under the cap loads.
        assert_eq!(
            fetch_feed(&test_client(), &url, 4096).await.unwrap().len(),
            2048
        );
    }

    #[tokio::test]
    async fn test_fetch_caps_streamed_body_without_length() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Chunked answer with no Content-Length: the cap must hold while streaming.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let mut resp = String::from("HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n");
            for _ in 0..8 {
                resp.push_str(&format!("200\r\n{}\r\n", "y".repeat(0x200)));
            }
            resp.push_str("0\r\n\r\n");
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        let err = fetch_feed(&test_client(), &format!("http://{addr}/stream"), 1024)
            .await
            .unwrap_err();
        assert!(matches!(err, FeedError::TooLarge(1024)), "{err}");
    }

    #[tokio::test]
    async fn test_refresh_keeps_last_good_entries_of_a_failed_feed() {
        let server = wiremock::MockServer::start().await;
        let mount = |p: &'static str, status: u16, body: &'static str| {
            wiremock::Mock::given(wiremock::matchers::path(p))
                .respond_with(wiremock::ResponseTemplate::new(status).set_body_string(body))
        };
        mount("/a", 200, "0.0.0.0 a1.example\n0.0.0.0 a2.example\n")
            .mount(&server)
            .await;
        mount("/b", 200, "b1.example\n").mount(&server).await;
        let feeds = [
            feed(FeedFormat::Hosts, format!("{}/a", server.uri())),
            feed(FeedFormat::Domains, format!("{}/b", server.uri())),
        ];
        let mut sources = feeds_only(&feeds, FeedLimits::default());
        let client = test_client();

        let first = sources.refresh(&client, false).await;
        assert_eq!(first.failed, 0);
        assert_eq!(first.merged.len(), 3);

        // Feed a changes, feed b fails: b keeps its last-known-good entry.
        server.reset().await;
        mount("/a", 200, "0.0.0.0 a3.example\n")
            .mount(&server)
            .await;
        mount("/b", 500, "").mount(&server).await;
        let second = sources.refresh(&client, false).await;
        assert_eq!(second.failed, 1);
        let mut got: Vec<_> = second.merged.into_iter().collect();
        got.sort();
        assert_eq!(got, ["a3.example", "b1.example"]);

        // A feed that never loaded adds nothing and does not block the others.
        let never = [
            feed(FeedFormat::Hosts, format!("{}/a", server.uri())),
            feed(FeedFormat::Domains, format!("{}/missing", server.uri())),
        ];
        let mut sources = feeds_only(&never, FeedLimits::default());
        let outcome = sources.refresh(&client, false).await;
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.merged.len(), 1);
    }

    #[tokio::test]
    async fn test_refresh_missing_seed_file_is_not_a_failure() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::path("/a"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("a1.example\n"))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let seed = Source {
            name: "seed".into(),
            kind: SourceKind::Seed(dir.path().join("missing.txt").display().to_string()),
            last_good: None,
        };
        let feeds = [feed(FeedFormat::Domains, format!("{}/a", server.uri()))];
        let mut sources = RefreshSources::with_sources(vec![seed], &feeds, FeedLimits::default());

        let outcome = sources.refresh(&test_client(), false).await;
        assert_eq!(outcome.failed, 0);
        assert_eq!(outcome.merged.len(), 1);
    }

    #[tokio::test]
    async fn test_refresh_enforces_aggregate_budget() {
        let server = wiremock::MockServer::start().await;
        for (p, body) in [
            ("/a", "a1.example\na2.example\na3.example\na4.example\n"),
            ("/b", "b1.example\nb2.example\nb3.example\n"),
            ("/c", "c1.example\n"),
        ] {
            wiremock::Mock::given(wiremock::matchers::path(p))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
                .mount(&server)
                .await;
        }
        let feeds = [
            feed(FeedFormat::Domains, format!("{}/a", server.uri())),
            feed(FeedFormat::Domains, format!("{}/b", server.uri())),
            feed(FeedFormat::Domains, format!("{}/c", server.uri())),
        ];
        let limits = FeedLimits {
            per_source: 3,
            total: 5,
            max_bytes: 1024,
        };
        let mut sources = feeds_only(&feeds, limits);
        let client = test_client();

        // a: 3 of 4 (per-source cap), b: 2 of 3 (budget left), c: nothing left.
        let outcome = sources.refresh(&client, false).await;
        assert_eq!(outcome.merged.len(), 5);
        let held: usize = sources.sources.iter().map(Source::len).sum();
        assert_eq!(held, 5);
        assert_eq!(sources.sources[2].len(), 0);

        // The budget holds on every later cycle too.
        let outcome = sources.refresh(&client, false).await;
        assert_eq!(outcome.merged.len(), 5);
        let held: usize = sources.sources.iter().map(Source::len).sum();
        assert!(held <= limits.total);
    }

    #[tokio::test]
    async fn test_disabled_blocklist() {
        let bl = Blocklist::disabled();
        assert!(!bl.is_blocked("evil.tk").await.is_blocked());
        assert!(!bl.is_blocked("192.168.1.1").await.is_blocked());
    }
}
