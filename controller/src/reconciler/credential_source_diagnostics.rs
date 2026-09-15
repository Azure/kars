// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Project only fixed failure categories and HTTP status from governed input errors.

pub(super) fn classify(reason: &str) -> (&'static str, Option<u16>) {
    let category = match reason {
        "Credential bindings require the exact workspace grant and one to three sources"
        | "Credential sources must be unique and ordered workspace, Team, target with explicit safe key grants" => {
            "binding_shape"
        }
        "An exact workspace credential grant is required"
        | "Credential grant is stale, unready, or replaced" => "grant_identity",
        "Credential target was replaced"
        | "Credential target requires a complete supported UID-bound identity" => "target_identity",
        "Credential owner is not the target"
        | "Credential owners cannot cross workspaces"
        | "Credential owner is outside the authorized ancestry" => "owner_authority",
        "Selected credential source was replaced" => "source_identity",
        "Source purpose, target, workspace, type or grant identity is invalid" => "source_metadata",
        "Credential source key grant or exact owner does not match"
        | "Credential source has a foreign owner; it is not adopted" => "source_owner",
        "Legacy credentials require explicit operator UID/resourceVersion/key-name review before source migration" => {
            "legacy_review"
        }
        "Existing credential bundle is not owned by the exact target" => "bundle_owner",
        "Previously bound credential bundle disappeared; explicit operator recovery is required" => {
            "bundle_missing"
        }
        "Credential target changed before bundle write" => "target_changed",
        "Credential grant changed before bundle write" => "grant_changed",
        "Credential source changed before bundle write"
        | "Credential source changed during read" => "source_changed",
        _ => "unclassified",
    };
    if category != "unclassified" {
        return (category, None);
    }
    for (stage, category) in [
        ("Read credential grant", "grant_api"),
        ("Recheck live credential grant", "grant_api"),
        ("Verify credential workspace", "namespace_api"),
        ("Read credential target", "target_api"),
        ("Read selected source identity", "source_metadata_api"),
        ("Read selected agent credentials", "source_value_api"),
        ("Read owned credential bundle", "bundle_read_api"),
        ("Create owned credential bundle anchor", "bundle_create_api"),
        (
            "Record actual credential bundle CREATE UID",
            "bundle_bind_api",
        ),
        ("Write UID-fenced credential bundle", "bundle_write_api"),
        ("Recheck credential source", "source_recheck_api"),
        ("Bind source to captured target UID", "source_bind_api"),
        (
            "Import only reviewed legacy credential keys",
            "legacy_import_api",
        ),
        ("Inspect legacy credential identity", "legacy_identity_api"),
        (
            "Inspect legacy credential namespace",
            "legacy_namespace_api",
        ),
        ("Inspect legacy credential key names", "legacy_metadata_api"),
        (
            "Verify selected legacy credential owner",
            "legacy_target_api",
        ),
        ("Read reviewed legacy credentials", "legacy_value_api"),
    ] {
        let Some(detail) = reason
            .strip_prefix(stage)
            .and_then(|value| value.strip_prefix(": "))
        else {
            continue;
        };
        if detail == "Kubernetes transport or serialization failure" {
            return (category, None);
        }
        if let Some(code) = detail
            .strip_prefix("Kubernetes status ")
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|code| (100..=599).contains(code))
        {
            return (category, Some(code));
        }
    }
    ("unclassified", None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governed_diagnostics_keep_only_known_category_and_http_status() {
        assert_eq!(
            classify("Read credential target: Kubernetes status 404"),
            ("target_api", Some(404))
        );
        assert_eq!(
            classify("Credential source key grant or exact owner does not match"),
            ("source_owner", None)
        );
        assert_eq!(
            classify(
                "Create owned credential bundle anchor: Kubernetes transport or serialization failure"
            ),
            ("bundle_create_api", None)
        );
        for text in [
            "PRIVATE_VALUE",
            "Read credential target: Kubernetes status 404 PRIVATE_VALUE",
            "Read credential target: Kubernetes status 999",
            "Read credential target PRIVATE_VALUE: Kubernetes status 404",
            "Credential source key grant or exact owner does not match PRIVATE_VALUE",
        ] {
            assert_eq!(classify(text), ("unclassified", None));
        }
    }
}
