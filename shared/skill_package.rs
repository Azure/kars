// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared package bytes and review binding; neither is a signature verification.

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const REVIEW: &str = "kars.azure.com/skill-review";
pub const LOCKED_DIGEST: &str = "kars.azure.com/skill-locked-digest";
pub const LOCKED_UID: &str = "kars.azure.com/skill-locked-uid";
pub const LOCKED_GENERATION: &str = "kars.azure.com/skill-locked-generation";
pub const APPROVED_BY_SUB: &str = "kars.azure.com/skill-approved-by-sub";
pub const UPLOADED_BY_SUB: &str = "kars.azure.com/skill-uploaded-by-sub";
pub const MAX_PACKAGE_BYTES: usize = 1024 * 1024;

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
}

pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 253
        && path.len() + path.bytes().filter(|b| *b == b'/').count() <= 253
        && !path.contains("..")
        && !path.contains("__")
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

pub fn validate_manifest(paths: &[String]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for path in paths {
        if !valid_path(path) || !seen.insert(path.as_str()) {
            return Err(format!("Invalid or duplicate skill file path '{path}'"));
        }
    }
    if !seen.contains("SKILL.md") {
        return Err("A skill package must contain SKILL.md".into());
    }
    for path in paths {
        for (index, _) in path.match_indices('/') {
            if seen.contains(&path[..index]) {
                return Err(format!("Skill file '{path}' conflicts with a parent file"));
            }
        }
    }
    Ok(())
}

pub fn valid_digest(digest: &str) -> bool {
    digest.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub fn package_digest(files: &BTreeMap<String, String>) -> Result<String, String> {
    let bytes =
        serde_json::to_vec(files).map_err(|e| format!("Skill package encoding failed: {e}"))?;
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err("Skill package exceeds the 1 MiB ConfigMap data bound".into());
    }
    Ok(format!(
        "sha256:{}",
        crate::providers::signing::sha256_hex(&bytes)
    ))
}

pub fn validate_package(
    paths: &[String],
    encoded_files: &BTreeMap<String, String>,
    expected_digest: &str,
) -> Result<(), String> {
    validate_manifest(paths)?;
    let expected_keys: BTreeSet<_> = paths.iter().map(|p| p.replace('/', "__")).collect();
    if expected_keys != encoded_files.keys().cloned().collect() {
        return Err("Skill package bytes do not match its exact file manifest".into());
    }
    if !valid_digest(expected_digest) || package_digest(encoded_files)? != expected_digest {
        return Err("Skill package digest does not match its file bytes".into());
    }
    Ok(())
}

pub fn validate_owned_package(skill: &Value, package: &Value) -> Result<(), String> {
    let metadata = &skill["metadata"];
    let uid = metadata["uid"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("Skill UID is unavailable")?;
    let name = metadata["name"]
        .as_str()
        .filter(|s| valid_name(s))
        .ok_or("Skill name is invalid")?;
    let namespace = metadata["namespace"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("Skill namespace is unavailable")?;
    let owner = &package["metadata"];
    let controllers: Vec<_> = owner["ownerReferences"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["controller"].as_bool() == Some(true))
        .collect();
    if skill["spec"]["package"].as_bool() != Some(true)
        || owner["name"].as_str() != Some(format!("karsskill-{name}").as_str())
        || owner["namespace"].as_str() != Some(namespace)
        || owner.get("deletionTimestamp").is_some_and(|v| !v.is_null())
        || package
            .get("binaryData")
            .is_some_and(|v| !v.is_null() && v.as_object().is_none_or(|v| !v.is_empty()))
        || controllers.len() != 1
        || controllers[0]["apiVersion"].as_str() != Some("kars.azure.com/v1alpha1")
        || controllers[0]["kind"].as_str() != Some("KarsSkill")
        || controllers[0]["name"].as_str() != Some(name)
        || controllers[0]["uid"].as_str() != Some(uid)
    {
        return Err("Skill package is missing exact same-namespace Skill UID ownership".into());
    }
    let files: BTreeMap<String, String> = serde_json::from_value(package["data"].clone())
        .map_err(|_| "Skill package has invalid file data")?;
    let paths: Vec<String> = serde_json::from_value(skill["spec"]["files"].clone())
        .map_err(|_| "Skill package has invalid file paths")?;
    validate_package(
        &paths,
        &files,
        skill["spec"]["packageDigest"]
            .as_str()
            .ok_or("Skill packageDigest is missing")?,
    )
}

pub fn validate_spec(spec: &Value) -> Result<(), String> {
    for key in ["summary", "version", "boundingPolicy"] {
        if spec
            .get(key)
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err(format!("Skill {key} must be a nonempty string"));
        }
    }
    if spec.get("package").and_then(Value::as_bool) == Some(true) {
        let paths: Vec<String> = serde_json::from_value(spec["files"].clone())
            .map_err(|_| "Skill files must be an array of relative paths")?;
        validate_manifest(&paths)?;
        let digest = spec["packageDigest"]
            .as_str()
            .ok_or("Skill packageDigest is missing")?;
        if !valid_digest(digest) {
            return Err("Skill packageDigest must be a full lowercase SHA256 digest".into());
        }
    } else if spec.get("packageDigest").is_some_and(|v| !v.is_null())
        || spec
            .get("files")
            .is_some_and(|v| v.as_array().is_none_or(|v| !v.is_empty()))
    {
        return Err("Skill package files and digest require package=true".into());
    }
    Ok(())
}

pub fn version_digest(spec: &Value) -> String {
    // Serialize fields directly so serde_json's preserve_order feature cannot
    // change the controller's existing wire encoding in another consumer.
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Content {
        summary: Value,
        version: Value,
        bounding_policy: Value,
        mcp_servers: Value,
        recipe: Value,
        knowledge_pack: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        package: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        files: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        package_digest: Option<Value>,
    }
    let packaged = spec.get("package").and_then(Value::as_bool) == Some(true);
    let canonical = Content {
        summary: spec["summary"].clone(),
        version: spec["version"].clone(),
        bounding_policy: spec["boundingPolicy"].clone(),
        mcp_servers: spec.get("mcpServers").cloned().unwrap_or_else(|| json!([])),
        recipe: spec.get("recipe").cloned().unwrap_or(Value::Null),
        knowledge_pack: spec.get("knowledgePack").cloned().unwrap_or(Value::Null),
        package: packaged.then_some(true),
        files: packaged.then(|| spec["files"].clone()),
        package_digest: packaged.then(|| spec["packageDigest"].clone()),
    };
    let bytes = serde_json::to_vec(&canonical).expect("Skill content contains only JSON values");
    // Preserve the existing recipe-only 128-bit identifier. Package bytes have
    // their own full SHA256, included in this version binding.
    format!(
        "sha256:{}",
        &crate::providers::signing::sha256_hex(&bytes)[..32]
    )
}

pub fn validated_digest(object: &Value) -> Result<String, String> {
    let metadata = &object["metadata"];
    let generation = metadata["generation"]
        .as_i64()
        .filter(|g| *g > 0)
        .ok_or("Skill generation is unavailable")?;
    if metadata
        .get("deletionTimestamp")
        .is_some_and(|v| !v.is_null())
        || metadata["uid"].as_str().is_none_or(str::is_empty)
        || metadata["resourceVersion"]
            .as_str()
            .is_none_or(str::is_empty)
        || object["status"]["phase"].as_str() != Some("Ready")
        || object["status"]["observedGeneration"].as_i64() != Some(generation)
    {
        return Err("Skill is missing, terminating, or not currently validated".into());
    }
    validate_spec(&object["spec"])?;
    let digest = version_digest(&object["spec"]);
    if object["status"]["versionDigest"].as_str() != Some(&digest) {
        return Err("Skill content changed since controller validation".into());
    }
    Ok(digest)
}

pub fn approved_digest(object: &Value) -> Result<String, String> {
    let digest = validated_digest(object)?;
    let metadata = &object["metadata"];
    let annotations = &metadata["annotations"];
    let generation = metadata["generation"]
        .as_i64()
        .ok_or("Skill generation is unavailable")?;
    let reviewer = annotations[APPROVED_BY_SUB]
        .as_str()
        .filter(|s| !s.is_empty());
    let author = annotations[UPLOADED_BY_SUB]
        .as_str()
        .filter(|s| !s.is_empty());
    if annotations[REVIEW].as_str() != Some("approved")
        || annotations[LOCKED_DIGEST].as_str() != Some(&digest)
        || annotations[LOCKED_UID].as_str() != metadata["uid"].as_str()
        || annotations[LOCKED_GENERATION].as_str() != Some(&generation.to_string())
        || reviewer.is_none()
        || author.is_none()
        || reviewer == author
    {
        return Err("Skill needs independent operator approval bound to its current UID, generation and content".into());
    }
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_version_digest_preserves_controller_encoding_across_json_features() {
        let recipe = json!({
            "boundingPolicy":"kars-default", "version":"1.0.0",
            "summary":"Check local inputs."
        });
        assert_eq!(
            version_digest(&recipe),
            "sha256:98698de66277886a1dc35e2074b9b9d2"
        );
        let packaged = json!({
            "boundingPolicy":"kars-default",
            "displayName":"Beta checklist quality",
            "files":["SKILL.md"],
            "package":true,
            "packageDigest":"sha256:b118da418fccf64b37054a7210f11515538bde5bc40a0ae7a8050b51b21ed539",
            "summary":"Check a local markdown checklist for consent, accessibility and privacy without network access.",
            "version":"1.0.0"
        });
        assert_eq!(
            version_digest(&packaged),
            "sha256:81bf4b81d36e607d6470099f952ed8f6"
        );
        let reversed: Value = serde_json::from_str(
            r#"{"version":"1.0.0","summary":"Check local inputs.","boundingPolicy":"kars-default"}"#,
        )
        .unwrap();
        assert_eq!(version_digest(&recipe), version_digest(&reversed));
    }

    #[test]
    fn skill_fixture_matches_both_runtime_codecs_and_exact_owner() {
        let files: BTreeMap<String, String> =
            serde_json::from_str(include_str!("../tests/fixtures/skill-package.json")).unwrap();
        let encoded: BTreeMap<_, _> = files
            .iter()
            .map(|(path, content)| (path.replace('/', "__"), content.clone()))
            .collect();
        let digest = package_digest(&encoded).unwrap();
        assert_eq!(
            digest,
            "sha256:ff733becaecde141e87b4b8223ca99091bb181295398a1ddace419976dd16615"
        );
        let skill = json!({
            "metadata":{"name":"checklist","namespace":"work","uid":"skill-uid"},
            "spec":{"package":true,"files":files.keys().collect::<Vec<_>>(),"packageDigest":digest}
        });
        let package = json!({
            "metadata":{"name":"karsskill-checklist","namespace":"work",
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSkill",
                    "name":"checklist","uid":"skill-uid","controller":true}]},
            "data":encoded
        });
        assert!(validate_owned_package(&skill, &package).is_ok());
        for (pointer, value) in [
            ("/metadata/name", json!("other")),
            ("/metadata/namespace", json!("other")),
            ("/metadata/ownerReferences/0/uid", json!("old-uid")),
            ("/metadata/ownerReferences/0/name", json!("other")),
            ("/metadata/ownerReferences/0/kind", json!("KarsSandbox")),
            ("/metadata/ownerReferences/0/apiVersion", json!("v1")),
            ("/metadata/ownerReferences/0/controller", json!(false)),
            ("/data/2", json!("changed")),
        ] {
            let mut changed = package.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                validate_owned_package(&skill, &changed).is_err(),
                "{pointer}"
            );
        }
        let mut changed = package.clone();
        changed["metadata"]["deletionTimestamp"] = json!("2026-10-09T03:00:00Z");
        assert!(validate_owned_package(&skill, &changed).is_err());
        changed = package.clone();
        changed["binaryData"] = json!({"hidden":"YQ=="});
        assert!(validate_owned_package(&skill, &changed).is_err());
        assert!(!valid_path(&("x/".repeat(85) + "z")));
        assert!(valid_path(&"a".repeat(253)));
    }

    fn reviewed() -> Value {
        let mut object = json!({
            "metadata": {"uid":"skill-uid","resourceVersion":"11","generation":2,
                "annotations":{
                    REVIEW:"approved", LOCKED_UID:"skill-uid", LOCKED_GENERATION:"2",
                    UPLOADED_BY_SUB:"author", APPROVED_BY_SUB:"reviewer"
                }},
            "spec":{"summary":"Review local documents","version":"1","boundingPolicy":"read-only"},
            "status":{"phase":"Ready","observedGeneration":2}
        });
        let digest = version_digest(&object["spec"]);
        object["status"]["versionDigest"] = json!(digest);
        object["metadata"]["annotations"][LOCKED_DIGEST] = json!(digest);
        object
    }

    #[test]
    fn package_manifest_and_bytes_are_exact() {
        let paths = vec!["SKILL.md".into(), "references/checklist.md".into()];
        let files = BTreeMap::from([
            (
                "SKILL.md".into(),
                "---\nname: review\ndescription: Review documents\n---\n".into(),
            ),
            (
                "references__checklist.md".into(),
                "Check consent and accessibility.\n".into(),
            ),
        ]);
        let digest = package_digest(&files).unwrap();
        assert!(validate_package(&paths, &files, &digest).is_ok());
        let mut changed = files.clone();
        changed.insert(
            "references__checklist.md".into(),
            "Different content".into(),
        );
        assert!(validate_package(&paths, &changed, &digest).is_err());
        changed = files.clone();
        changed.insert("unexpected".into(), "extra".into());
        assert!(validate_package(&paths, &changed, &digest).is_err());
        for bad in [
            ".",
            "../secret",
            "/absolute",
            "a/./b",
            "a//b",
            "a__b",
            "a/",
            "a\\b",
        ] {
            assert!(!valid_path(bad), "{bad}");
        }
        for invalid in [
            vec!["SKILL.md".into(), "SKILL.md".into()],
            vec![
                "SKILL.md".into(),
                "references".into(),
                "references/x.md".into(),
            ],
            vec!["other.md".into()],
        ] {
            assert!(validate_manifest(&invalid).is_err());
        }
        assert!(!valid_digest(&format!("sha256:{}", "g".repeat(64))));
        assert!(
            package_digest(&BTreeMap::from([(
                "SKILL.md".into(),
                "x".repeat(MAX_PACKAGE_BYTES)
            )]))
            .is_err()
        );
    }

    #[test]
    fn approval_rejects_stale_replaced_unready_and_self_reviewed_skills() {
        let object = reviewed();
        assert!(approved_digest(&object).is_ok());
        for (pointer, value) in [
            ("/metadata/uid", json!("replacement")),
            ("/metadata/generation", json!(3)),
            ("/metadata/resourceVersion", json!("")),
            ("/status/phase", json!("Degraded")),
            ("/status/observedGeneration", json!(1)),
            ("/spec/summary", json!("Changed content")),
        ] {
            let mut bad = object.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(approved_digest(&bad).is_err(), "{pointer}");
        }
        for (key, value) in [
            (REVIEW, "pending"),
            (LOCKED_DIGEST, "wrong"),
            (LOCKED_UID, "other"),
            (LOCKED_GENERATION, "1"),
            (APPROVED_BY_SUB, "author"),
            (UPLOADED_BY_SUB, ""),
        ] {
            let mut bad = object.clone();
            bad["metadata"]["annotations"][key] = json!(value);
            assert!(approved_digest(&bad).is_err(), "{key}");
        }
        let mut deleted = object;
        deleted["metadata"]["deletionTimestamp"] = json!("2026-10-09T00:00:00Z");
        assert!(approved_digest(&deleted).is_err());
    }
}
