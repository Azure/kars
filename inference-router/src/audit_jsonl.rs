// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Durable JSONL audit-trail writer — Slice 4 DoD #4 ("audit rows persist
//! across router restart").
//!
//! [`JsonlAuditWriter`] is the sandbox-local sink invoked from every
//! [`crate::governance::Governance::audit_log`] call. It appends one JSON
//! object per line to `{dir}/YYYY-MM-DD.jsonl`, rotating files at UTC date
//! boundaries derived from the [`agentmesh::AuditEntry::timestamp`] string
//! (no clock injection needed — the SDK already stamps each entry).
//!
//! ## Contract
//!
//! - **One line per append.** `serde_json::to_string` + `\n`; the file is
//!   opened once per rotation in `append` mode (`O_APPEND` on Linux is
//!   atomic across processes for writes ≤ `PIPE_BUF`, comfortably above
//!   our line sizes).
//! - **Rotation by date key.** Date key is `entry.timestamp[..10]`
//!   (`"YYYY-MM-DD"`). Mismatch with the cached date key triggers a
//!   close+reopen on the next file.
//! - **Non-fatal on I/O error.** The caller logs a warn and continues —
//!   audit persistence failing must not deny in-flight requests. The
//!   in-memory chain remains the authoritative short-term log.
//! - **Sandbox name is denormalised.** Every record carries `sandbox` so
//!   `kars audit tail` can multiplex multi-sandbox forensic queries
//!   without parsing the file path.
//!
//! ## Why not a tokio task?
//!
//! The router's audit path is already on the caller's tokio runtime and
//! the write volume is bounded (≤ a few hundred entries/second under
//! load). A synchronous `OpenOptions::append` + `write_all` is fine and
//! avoids an unbounded channel / extra task lifecycle. If profile data
//! ever shows this contended we'll move to a bounded MPSC drained by a
//! background task — but **not** speculatively (principles §5).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;

/// Sandbox-local JSONL audit writer.
///
/// One instance per [`crate::governance::Governance`]. Cloning is not
/// supported by design — the writer owns its file handle.
pub struct JsonlAuditWriter {
    dir: PathBuf,
    sandbox: String,
    state: Mutex<Option<RotationState>>,
}

struct RotationState {
    date_key: String,
    file: File,
}

#[derive(Serialize)]
struct JsonlRecord<'a> {
    sandbox: &'a str,
    seq: u64,
    ts: &'a str,
    agent_id: &'a str,
    action: &'a str,
    decision: &'a str,
    prev_hash: &'a str,
    hash: &'a str,
}

impl JsonlAuditWriter {
    /// Construct a writer rooted at `dir`. Creates the directory tree if
    /// missing. Returns an error if directory creation fails (e.g.
    /// permission denied) — the caller should warn-log and continue
    /// without a sink rather than panic.
    pub fn try_new(dir: impl AsRef<Path>, sandbox: impl Into<String>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            sandbox: sandbox.into(),
            state: Mutex::new(None),
        })
    }

    /// Append one record. Date key derives from `entry.timestamp[..10]`;
    /// rotation happens lazily on mismatch.
    pub fn write(&self, entry: &agentmesh::AuditEntry) -> io::Result<()> {
        let date_key = date_key_for(&entry.timestamp);
        let record = JsonlRecord {
            sandbox: &self.sandbox,
            seq: entry.seq,
            ts: &entry.timestamp,
            agent_id: &entry.agent_id,
            action: &entry.action,
            decision: &entry.decision,
            prev_hash: &entry.previous_hash,
            hash: &entry.hash,
        };
        let mut line = serde_json::to_string(&record)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let needs_open = match guard.as_ref() {
            Some(st) => st.date_key != date_key,
            None => true,
        };
        if needs_open {
            let path = self.dir.join(format!("{date_key}.jsonl"));
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            *guard = Some(RotationState {
                date_key: date_key.clone(),
                file,
            });
        }
        let st = guard
            .as_mut()
            .expect("rotation state must exist after open");
        st.file.write_all(line.as_bytes())?;
        // No explicit flush — `O_APPEND` writes hit the page cache
        // immediately and the kernel is responsible for durability under
        // pod-level shutdown. Forcing fsync per entry would 10x the
        // wall-clock cost of every audited call.
        Ok(())
    }

    /// Resolve the on-disk path for a given date key. Exposed for tests
    /// and the `kars audit tail` CLI (Slice 4b) which needs to
    /// enumerate the directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Extract the `YYYY-MM-DD` prefix from an ISO-8601 timestamp string.
/// Returns `"unknown"` when the input is malformed — keeps rotation
/// deterministic even if the SDK ever emits a non-ISO timestamp.
fn date_key_for(timestamp: &str) -> String {
    timestamp
        .get(..10)
        .filter(|s| {
            let bytes = s.as_bytes();
            bytes.len() == 10
                && bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes[..4].iter().all(u8::is_ascii_digit)
                && bytes[5..7].iter().all(u8::is_ascii_digit)
                && bytes[8..].iter().all(u8::is_ascii_digit)
        })
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn entry(seq: u64, ts: &str) -> agentmesh::AuditEntry {
        agentmesh::AuditEntry {
            seq,
            timestamp: ts.into(),
            agent_id: "agent-1".into(),
            action: "tool.invoke".into(),
            decision: "allow".into(),
            previous_hash: if seq == 0 { "".into() } else { "abcd".into() },
            hash: format!("hash-{seq}"),
        }
    }

    fn tmp_dir(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "kars-jsonl-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn date_key_extracts_iso_prefix() {
        assert_eq!(date_key_for("2026-05-13T18:00:00Z"), "2026-05-13");
        assert_eq!(date_key_for("2026-05-13T18:00:00.123Z"), "2026-05-13");
        assert_eq!(date_key_for("2026-05-13T18:00:00+02:00"), "2026-05-13");
    }

    #[test]
    fn date_key_rejects_malformed() {
        assert_eq!(date_key_for("bogus"), "unknown");
        assert_eq!(date_key_for("2026/05/13"), "unknown");
        assert_eq!(date_key_for(""), "unknown");
        assert_eq!(date_key_for("2026-XX-13T00:00:00Z"), "unknown");
    }

    #[test]
    fn write_appends_one_line_per_entry() {
        let dir = tmp_dir("append");
        let w = JsonlAuditWriter::try_new(&dir, "test-sandbox").unwrap();
        w.write(&entry(0, "2026-05-13T10:00:00Z")).unwrap();
        w.write(&entry(1, "2026-05-13T10:00:01Z")).unwrap();

        let path = dir.join("2026-05-13.jsonl");
        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "expected two lines, got: {contents:?}");
        assert!(lines[0].contains("\"seq\":0"));
        assert!(lines[0].contains("\"sandbox\":\"test-sandbox\""));
        assert!(lines[0].contains("\"hash\":\"hash-0\""));
        assert!(lines[1].contains("\"seq\":1"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_rotates_file_at_date_boundary() {
        let dir = tmp_dir("rotate");
        let w = JsonlAuditWriter::try_new(&dir, "test-sandbox").unwrap();
        w.write(&entry(0, "2026-05-13T23:59:59Z")).unwrap();
        w.write(&entry(1, "2026-05-14T00:00:00Z")).unwrap();
        w.write(&entry(2, "2026-05-14T00:00:01Z")).unwrap();

        let day1 = fs::read_to_string(dir.join("2026-05-13.jsonl")).unwrap();
        let day2 = fs::read_to_string(dir.join("2026-05-14.jsonl")).unwrap();
        assert_eq!(day1.lines().count(), 1);
        assert_eq!(day2.lines().count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_timestamp_falls_back_to_unknown_jsonl() {
        let dir = tmp_dir("unknown");
        let w = JsonlAuditWriter::try_new(&dir, "test-sandbox").unwrap();
        w.write(&entry(0, "not-a-timestamp")).unwrap();
        assert!(dir.join("unknown.jsonl").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn line_is_valid_json_with_expected_fields() {
        let dir = tmp_dir("schema");
        let w = JsonlAuditWriter::try_new(&dir, "sb").unwrap();
        w.write(&entry(0, "2026-05-13T00:00:00Z")).unwrap();
        let path = dir.join("2026-05-13.jsonl");
        let contents = fs::read_to_string(&path).unwrap();
        let line = contents.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        for key in [
            "sandbox",
            "seq",
            "ts",
            "agent_id",
            "action",
            "decision",
            "prev_hash",
            "hash",
        ] {
            assert!(v.get(key).is_some(), "missing field {key} in {line}");
        }
        assert_eq!(v["sandbox"], "sb");
        assert_eq!(v["seq"], 0);
        assert_eq!(v["agent_id"], "agent-1");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writer_creates_missing_directory() {
        let dir = tmp_dir("missing").join("nested").join("deeper");
        assert!(!dir.exists());
        let w = JsonlAuditWriter::try_new(&dir, "sb").unwrap();
        w.write(&entry(0, "2026-05-13T00:00:00Z")).unwrap();
        assert!(dir.join("2026-05-13.jsonl").exists());
        let _ = fs::remove_dir_all(dir.ancestors().nth(2).unwrap());
    }

    #[test]
    fn directory_accessor_returns_root() {
        let dir = tmp_dir("accessor");
        let w = JsonlAuditWriter::try_new(&dir, "sb").unwrap();
        assert_eq!(w.dir(), dir.as_path());
        let _ = fs::remove_dir_all(&dir);
    }
}
