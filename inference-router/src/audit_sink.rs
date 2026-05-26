// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Audit sink trait + implementations (Slice 4c).
//!
//! Slice 4a gave the router a sandbox-local JSONL audit file. Slice 4b
//! gave operators a way to read it. This slice gives the router a way
//! to *also* ship those rows to a remote sink for centralised forensics
//! and long-term retention, satisfying Slice 4 DoD #5.
//!
//! ## Why a trait
//!
//! Per `docs/internal/crd-well-oiled-machine/slice-4-mcp-server-plural.md`
//! §5, `Governance` should fan an audit row out to multiple sinks at
//! once. The local JSONL file is always-on; remote sinks (Azure Monitor
//! Logs Ingestion today; Loki and Azure Storage in follow-ups) are
//! additive when configured. A trait `AuditSink` lets `Governance` hold
//! exactly one `Arc<dyn AuditSink>` regardless of how many concrete
//! sinks are wired in.
//!
//! ## Why the trait is sync
//!
//! `Governance::audit_log()` is called from ~10 callsites scattered
//! across `governance/mod.rs`, `governance/trust_ops.rs`, and
//! `providers/audit_impl.rs`. Most of those callsites are themselves
//! inside fast-path policy decisions where adding `.await` would force
//! a cascading async-ification of helper functions whose only job is to
//! return an `Outcome` enum. Audit must not block — so:
//!
//! - **`LocalJsonlSink`** writes synchronously. The Slice 4a writer
//!   already does this; appends are ≤ PIPE_BUF (4096 B) and atomic on
//!   Linux. Disk I/O latency is acceptable on the audit path.
//! - **`AzureMonitorSink`** serialises the row, then `tokio::spawn`s a
//!   one-shot HTTPS POST. The audit row is durably mirrored to JSONL
//!   regardless of whether the upstream POST succeeds. If Azure Monitor
//!   is unreachable, we log at `warn!` and move on; the request is not
//!   denied. (A future Slice 4c.2 can add an in-process batcher with
//!   bounded backpressure if per-row HTTP becomes a cost issue.)

use std::sync::Arc;

use serde::Serialize;

/// Trait every audit sink implements. Sync `write` so the `Governance`
/// audit chokepoint stays sync; sinks that need network I/O `tokio::spawn`
/// internally.
pub trait AuditSink: Send + Sync {
    /// Append one audit entry. Implementations must not panic. Errors
    /// are logged but never propagated — audit is never the reason a
    /// request is denied.
    fn write(&self, entry: &agentmesh::AuditEntry);
}

/// Fan-out wrapper that delivers every entry to all configured sinks.
/// Empty registry is a valid state (audit disabled) — `write` is a no-op.
pub struct CompositeSink {
    sinks: Vec<Arc<dyn AuditSink>>,
}

impl CompositeSink {
    pub fn new(sinks: Vec<Arc<dyn AuditSink>>) -> Self {
        Self { sinks }
    }

    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sinks.len()
    }
}

impl AuditSink for CompositeSink {
    fn write(&self, entry: &agentmesh::AuditEntry) {
        for sink in &self.sinks {
            sink.write(entry);
        }
    }
}

/// Local sandbox JSONL sink. Thin adapter over the Slice 4a writer so
/// the same code path can sit behind the `AuditSink` trait.
pub struct LocalJsonlSink {
    writer: crate::audit_jsonl::JsonlAuditWriter,
}

impl LocalJsonlSink {
    pub fn new(writer: crate::audit_jsonl::JsonlAuditWriter) -> Self {
        Self { writer }
    }
}

impl AuditSink for LocalJsonlSink {
    fn write(&self, entry: &agentmesh::AuditEntry) {
        if let Err(e) = self.writer.write(entry) {
            tracing::warn!(
                error = %e,
                "audit JSONL write failed (entry preserved in memory)"
            );
        }
    }
}

/// Configuration for the Azure Monitor Logs Ingestion sink.
///
/// All three fields are required. They come from a Data Collection
/// Endpoint + Data Collection Rule pair created out-of-band in the
/// operator's subscription. See
/// <https://learn.microsoft.com/azure/azure-monitor/logs/logs-ingestion-api-overview>
/// for the full ingestion model.
#[derive(Debug, Clone)]
pub struct AzureMonitorConfig {
    /// Data Collection Endpoint logs ingestion URL,
    /// e.g. `https://my-dce-xyz.eastus-1.ingest.monitor.azure.com`.
    pub dce_endpoint: String,
    /// Immutable ID of the Data Collection Rule, e.g. `dcr-1234...`.
    pub dcr_immutable_id: String,
    /// Custom log stream name declared in the DCR, e.g. `Custom-KarsAudit_CL`.
    pub stream_name: String,
    /// Sandbox name — included as a column on every row.
    pub sandbox: String,
}

impl AzureMonitorConfig {
    /// Build the ingestion URL: `{dce}/dataCollectionRules/{dcr}/streams/{stream}?api-version=2023-01-01`.
    pub fn ingestion_url(&self) -> String {
        let base = self.dce_endpoint.trim_end_matches('/');
        format!(
            "{base}/dataCollectionRules/{}/streams/{}?api-version=2023-01-01",
            self.dcr_immutable_id, self.stream_name
        )
    }
}

/// Wire shape of one row POSTed to Azure Monitor. The set of columns
/// must match the DCR's declared schema. Names use camelCase to fit
/// Log Analytics conventions.
#[derive(Serialize)]
pub(crate) struct AzMonRow<'a> {
    #[serde(rename = "TimeGenerated")]
    time_generated: &'a str,
    sandbox: &'a str,
    seq: u64,
    agent_id: &'a str,
    action: &'a str,
    decision: &'a str,
    prev_hash: &'a str,
    hash: &'a str,
}

impl<'a> AzMonRow<'a> {
    pub(crate) fn from_entry(sandbox: &'a str, entry: &'a agentmesh::AuditEntry) -> Self {
        Self {
            time_generated: &entry.timestamp,
            sandbox,
            seq: entry.seq,
            agent_id: &entry.agent_id,
            action: &entry.action,
            decision: &entry.decision,
            prev_hash: &entry.previous_hash,
            hash: &entry.hash,
        }
    }
}

/// Azure Monitor remote sink. Each call to `write` serialises the row
/// and spawns a single-row POST; failures are logged but otherwise
/// silent. The local JSONL sink is the authoritative durable record —
/// remote delivery is best-effort.
///
/// Token acquisition reuses `crate::auth::WorkloadIdentityAuth` so the
/// same managed-identity is presented for Foundry and Monitor; the
/// caller's UAMI must hold the `Monitoring Metrics Publisher` role on
/// the Data Collection Rule.
pub struct AzureMonitorSink {
    config: Arc<AzureMonitorConfig>,
    auth: Arc<crate::auth::WorkloadIdentityAuth>,
    client: reqwest::Client,
}

impl AzureMonitorSink {
    pub fn new(config: AzureMonitorConfig, auth: Arc<crate::auth::WorkloadIdentityAuth>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client build cannot fail");
        Self {
            config: Arc::new(config),
            auth,
            client,
        }
    }

    /// Serialise one row to the wire body Azure Monitor expects (a JSON
    /// array of objects). Exposed for unit tests.
    pub(crate) fn serialize_body(
        sandbox: &str,
        entry: &agentmesh::AuditEntry,
    ) -> Result<Vec<u8>, serde_json::Error> {
        let row = AzMonRow::from_entry(sandbox, entry);
        serde_json::to_vec(&[row])
    }
}

impl AuditSink for AzureMonitorSink {
    fn write(&self, entry: &agentmesh::AuditEntry) {
        let body = match Self::serialize_body(&self.config.sandbox, entry) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "azmon audit row serialization failed");
                return;
            }
        };
        let url = self.config.ingestion_url();
        let client = self.client.clone();
        let auth = self.auth.clone();
        tokio::spawn(async move {
            let token = match auth.get_token("https://monitor.azure.com/").await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "azmon token fetch failed");
                    return;
                }
            };
            let resp = client
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    tracing::trace!(status = %r.status(), "azmon audit row delivered");
                }
                Ok(r) => {
                    tracing::warn!(
                        status = %r.status(),
                        "azmon audit ingestion returned non-success status"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "azmon audit POST failed");
                }
            }
        });
    }
}

/// Build the sandbox-wide audit sink chain from environment variables.
///
/// - `KARS_AUDIT_DIR` controls the always-on local JSONL writer
///   (matches Slice 4a; value `"disabled"` or empty short-circuits).
/// - `KARS_AUDIT_AZMON_DCE`, `KARS_AUDIT_AZMON_DCR_ID`,
///   `KARS_AUDIT_AZMON_STREAM` together enable the Azure Monitor
///   sink. If any one is missing, the sink is disabled silently — this
///   is the configuration-by-env path that Slice 4d's plural McpServer
///   CRD will replace with a typed `AuditSink` enum.
///
/// Returns `None` when no sinks are configured (e.g. tests with
/// `KARS_AUDIT_DIR=disabled` and no AzMon env vars).
pub fn build_sink_from_env(
    sandbox_name: &str,
    auth: Arc<crate::auth::WorkloadIdentityAuth>,
) -> Option<Arc<dyn AuditSink>> {
    let mut sinks: Vec<Arc<dyn AuditSink>> = Vec::new();

    if let Some(writer) = open_local_jsonl_writer(sandbox_name) {
        sinks.push(Arc::new(LocalJsonlSink::new(writer)));
    }

    if let Some(cfg) = read_azmon_config_from_env(sandbox_name) {
        tracing::info!(
            sandbox = sandbox_name,
            dce = %cfg.dce_endpoint,
            dcr = %cfg.dcr_immutable_id,
            stream = %cfg.stream_name,
            "Azure Monitor audit sink enabled"
        );
        sinks.push(Arc::new(AzureMonitorSink::new(cfg, auth)));
    }

    if sinks.is_empty() {
        None
    } else {
        Some(Arc::new(CompositeSink::new(sinks)))
    }
}

fn open_local_jsonl_writer(sandbox_name: &str) -> Option<crate::audit_jsonl::JsonlAuditWriter> {
    let dir = std::env::var("KARS_AUDIT_DIR").unwrap_or_else(|_| "/var/log/kars/audit".into());
    if dir == "disabled" || dir.is_empty() {
        return None;
    }
    match crate::audit_jsonl::JsonlAuditWriter::try_new(&dir, sandbox_name) {
        Ok(w) => {
            tracing::info!(
                sandbox = sandbox_name,
                dir = %dir,
                "Durable audit JSONL writer initialized"
            );
            Some(w)
        }
        Err(e) => {
            tracing::warn!(
                sandbox = sandbox_name,
                dir = %dir,
                error = %e,
                "Failed to open audit JSONL writer — local mirror disabled"
            );
            None
        }
    }
}

/// Read Azure Monitor config from env. All three fields must be present
/// and non-empty for the sink to be enabled. This is the temporary
/// env-driven configuration path Slice 4c uses ahead of Slice 4d's
/// `McpServer.spec.auditSink` typed CRD field.
pub(crate) fn read_azmon_config_from_env(sandbox: &str) -> Option<AzureMonitorConfig> {
    let dce = non_empty_env("KARS_AUDIT_AZMON_DCE")?;
    let dcr = non_empty_env("KARS_AUDIT_AZMON_DCR_ID")?;
    let stream = non_empty_env("KARS_AUDIT_AZMON_STREAM")?;
    Some(AzureMonitorConfig {
        dce_endpoint: dce,
        dcr_immutable_id: dcr,
        stream_name: stream,
        sandbox: sandbox.to_string(),
    })
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn make_entry(seq: u64) -> agentmesh::AuditEntry {
        agentmesh::AuditEntry {
            seq,
            timestamp: "2026-05-13T11:22:33.456Z".to_string(),
            agent_id: "alpha".to_string(),
            action: "tool.call".to_string(),
            decision: "allowed".to_string(),
            previous_hash: "0".repeat(64),
            hash: "1".repeat(64),
        }
    }

    /// Minimal capture sink for verifying fanout behaviour.
    struct CountingSink {
        seqs: Mutex<Vec<u64>>,
    }
    impl CountingSink {
        fn new() -> Self {
            Self {
                seqs: Mutex::new(Vec::new()),
            }
        }
        fn snapshot(&self) -> Vec<u64> {
            self.seqs.lock().unwrap().clone()
        }
    }
    impl AuditSink for CountingSink {
        fn write(&self, entry: &agentmesh::AuditEntry) {
            self.seqs.lock().unwrap().push(entry.seq);
        }
    }

    #[test]
    fn composite_fans_out_to_every_sink() {
        let a = Arc::new(CountingSink::new());
        let b = Arc::new(CountingSink::new());
        let c = CompositeSink::new(vec![a.clone(), b.clone()]);
        c.write(&make_entry(1));
        c.write(&make_entry(2));
        assert_eq!(a.snapshot(), vec![1, 2]);
        assert_eq!(b.snapshot(), vec![1, 2]);
        assert_eq!(c.len(), 2);
        assert!(!c.is_empty());
    }

    #[test]
    fn composite_empty_is_noop() {
        let c = CompositeSink::new(Vec::new());
        // Must not panic.
        c.write(&make_entry(1));
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn local_jsonl_sink_writes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let writer = crate::audit_jsonl::JsonlAuditWriter::try_new(dir.path(), "sb").unwrap();
        let sink = LocalJsonlSink::new(writer);
        sink.write(&make_entry(42));
        let file = dir.path().join("2026-05-13.jsonl");
        let content = std::fs::read_to_string(file).unwrap();
        assert!(content.contains("\"seq\":42"));
        assert!(content.contains("\"sandbox\":\"sb\""));
    }

    #[test]
    fn azmon_url_composes_correctly() {
        let cfg = AzureMonitorConfig {
            dce_endpoint: "https://my-dce.eastus-1.ingest.monitor.azure.com".to_string(),
            dcr_immutable_id: "dcr-abcd1234".to_string(),
            stream_name: "Custom-KarsAudit_CL".to_string(),
            sandbox: "sb".to_string(),
        };
        assert_eq!(
            cfg.ingestion_url(),
            "https://my-dce.eastus-1.ingest.monitor.azure.com/dataCollectionRules/dcr-abcd1234/streams/Custom-KarsAudit_CL?api-version=2023-01-01"
        );
    }

    #[test]
    fn azmon_url_handles_trailing_slash() {
        let cfg = AzureMonitorConfig {
            dce_endpoint: "https://x.ingest.monitor.azure.com/".to_string(),
            dcr_immutable_id: "dcr-1".to_string(),
            stream_name: "Custom-Foo_CL".to_string(),
            sandbox: "sb".to_string(),
        };
        assert!(
            cfg.ingestion_url()
                .starts_with("https://x.ingest.monitor.azure.com/dataCollectionRules/")
        );
        // No double slash.
        assert!(!cfg.ingestion_url().contains(".com//"));
    }

    #[test]
    fn azmon_serializes_one_row_array() {
        let entry = make_entry(7);
        let body = AzureMonitorSink::serialize_body("sandbox-a", &entry).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        let row = &arr[0];
        assert_eq!(row["sandbox"], "sandbox-a");
        assert_eq!(row["seq"], 7);
        assert_eq!(row["TimeGenerated"], "2026-05-13T11:22:33.456Z");
        assert_eq!(row["agent_id"], "alpha");
        assert_eq!(row["action"], "tool.call");
        assert_eq!(row["decision"], "allowed");
    }

    #[test]
    fn azmon_config_env_requires_all_three() {
        // SAFETY: unsafe block required by Rust 2024 edition for std::env::set_var.
        // Tests in this module are not run in parallel because they mutate process env.
        unsafe {
            std::env::remove_var("KARS_AUDIT_AZMON_DCE");
            std::env::remove_var("KARS_AUDIT_AZMON_DCR_ID");
            std::env::remove_var("KARS_AUDIT_AZMON_STREAM");
        }
        assert!(read_azmon_config_from_env("sb").is_none());

        unsafe {
            std::env::set_var("KARS_AUDIT_AZMON_DCE", "https://x");
        }
        assert!(read_azmon_config_from_env("sb").is_none());

        unsafe {
            std::env::set_var("KARS_AUDIT_AZMON_DCR_ID", "dcr-1");
        }
        assert!(read_azmon_config_from_env("sb").is_none());

        unsafe {
            std::env::set_var("KARS_AUDIT_AZMON_STREAM", "Custom-Foo_CL");
        }
        let cfg = read_azmon_config_from_env("sb").unwrap();
        assert_eq!(cfg.dce_endpoint, "https://x");
        assert_eq!(cfg.dcr_immutable_id, "dcr-1");
        assert_eq!(cfg.stream_name, "Custom-Foo_CL");
        assert_eq!(cfg.sandbox, "sb");

        // Empty value → treated as missing.
        unsafe {
            std::env::set_var("KARS_AUDIT_AZMON_STREAM", "");
        }
        assert!(read_azmon_config_from_env("sb").is_none());

        unsafe {
            std::env::remove_var("KARS_AUDIT_AZMON_DCE");
            std::env::remove_var("KARS_AUDIT_AZMON_DCR_ID");
            std::env::remove_var("KARS_AUDIT_AZMON_STREAM");
        }
    }
}
