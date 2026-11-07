// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use anyhow::{Result, ensure};
use serde_json::Value;

/// Expand the stored input reference before applying the caller's custody checks.
/// Legacy records retain their independent candidate/assignment equality checks.
pub fn decode(raw: &str) -> Result<Value> {
    ensure!(
        raw.len() <= 900 * 1024,
        "Mission attempt exceeds storage bounds"
    );
    let mut record: Value = serde_json::from_str(raw)?;
    if let Some(reference) = record.get("inputArtifactsFromAssignment") {
        ensure!(
            reference == &Value::Bool(true),
            "Malformed mission input storage reference"
        );
        let inputs = record
            .get("assignment")
            .and_then(|a| a.get("inputArtifacts"))
            .filter(|v| v.is_array())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Missing referenced mission inputs"))?;
        let candidate = record
            .get_mut("candidate")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| anyhow::anyhow!("Missing mission candidate"))?;
        ensure!(
            !candidate.contains_key("inputArtifacts"),
            "Ambiguous mission input storage reference"
        );
        candidate.insert("inputArtifacts".into(), inputs);
        record
            .as_object_mut()
            .unwrap()
            .remove("inputArtifactsFromAssignment");
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_records_are_unchanged_including_conflicts_for_custody_validation() {
        let record = json!({"version":1,"candidate":{"inputArtifacts":["old"]},"assignment":{"inputArtifacts":["different"]}});
        assert_eq!(decode(&record.to_string()).unwrap(), record);
    }

    #[test]
    fn reference_restores_exact_inputs_without_changing_other_evidence() {
        let inputs = json!([{"content":"Café\r\n日本語 🌍","sha256":"unchanged","source":{"taskUid":"prior"}}]);
        let mut record = json!({"version":1,"candidate":{"taskUid":"current"},"assignment":{"inputArtifacts":inputs},
            "inputArtifactsFromAssignment":true,"reply":{"status":"succeeded"},"events":[{"status":"succeeded"}]});
        let decoded = decode(&record.to_string()).unwrap();
        record
            .as_object_mut()
            .unwrap()
            .remove("inputArtifactsFromAssignment");
        record["candidate"]["inputArtifacts"] = inputs;
        assert_eq!(decoded, record);
    }

    #[test]
    fn malformed_or_ambiguous_references_fail_closed() {
        for record in [
            json!({"inputArtifactsFromAssignment":false}),
            json!({"inputArtifactsFromAssignment":"true"}),
            json!({"inputArtifactsFromAssignment":null}),
            json!({"inputArtifactsFromAssignment":true,"candidate":{},"assignment":{}}),
            json!({"inputArtifactsFromAssignment":true,"candidate":[],"assignment":{"inputArtifacts":[]}}),
            json!({"inputArtifactsFromAssignment":true,"candidate":{},"assignment":{"inputArtifacts":null}}),
            json!({"inputArtifactsFromAssignment":true,"candidate":{"inputArtifacts":null},"assignment":{"inputArtifacts":[]}}),
            json!({"inputArtifactsFromAssignment":true,"candidate":{"inputArtifacts":[]},"assignment":{"inputArtifacts":[]}}),
        ] {
            assert!(decode(&record.to_string()).is_err(), "{record}");
        }
    }

    #[test]
    fn size_guard_applies_to_stored_bytes_not_expanded_duplicate() {
        let record = json!({"inputArtifactsFromAssignment":true,"candidate":{},
            "assignment":{"inputArtifacts":[{"content":"x".repeat(360 * 1024)}]},
            "reply":{"output":"y".repeat(240 * 1024)}});
        let raw = record.to_string();
        assert!(raw.len() < 900 * 1024);
        assert!(decode(&raw).unwrap().to_string().len() > 900 * 1024);
        assert!(decode(&format!("{}{}", raw, " ".repeat(900 * 1024))).is_err());
    }
}
