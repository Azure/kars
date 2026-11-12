// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde::Deserialize;

fn legacy_documents(text: &str) -> Vec<Value> {
    serde_yaml::Deserializer::from_str(text)
        .map(|doc| serde_yaml::Value::deserialize(doc).expect("legacy manifest must parse"))
        .filter(|v| !v.is_null())
        .map(|doc| serde_json::to_value(doc).expect("Kubernetes manifest must be JSON-compatible"))
        .collect()
}

#[test]
fn yaml_compatibility_static_crds() {
    let dir = repo_root().join("deploy/helm/kars/templates");
    let mut count = 0;
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry.expect("read manifest directory");
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_file() && name.starts_with("crd") && name.ends_with(".yaml") {
            let text = std::fs::read_to_string(entry.path()).expect("read CRD");
            assert_eq!(
                parse_manifests(&text).expect("maintained parser must read CRD"),
                legacy_documents(&text),
                "{}",
                entry.path().display()
            );
            count += 1;
        }
    }
    assert!(count >= 10, "must compare the full CRD corpus, got {count}");
}

#[test]
fn yaml_compatibility_rendered_chart() {
    match std::process::Command::new("helm").arg("version").output() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("helm unavailable; static CRD differential coverage still runs");
            return;
        }
        Err(error) => panic!("cannot run helm: {error}"),
        Ok(output) => assert!(output.status.success(), "helm version must succeed"),
    }
    let text =
        render_helm(&repo_root()).expect("installed Helm must render the chart successfully");
    let old = legacy_documents(&text);
    let new = parse_manifests(&text).expect("maintained parser must read rendered chart");
    assert!(!old.is_empty(), "rendered corpus must contain documents");
    assert_eq!(new, old);
}

#[test]
fn yaml_compatibility_streams_and_scalars() {
    for text in [
        "",
        "# comment only\n",
        "---\nkind: Test\n---\nnull\n---\nkind: Another\n",
        "metadata: {name: '01', labels: {enabled: 'true'}}\n",
        "yes: yes\nno: no\non: on\noff: off\nboolean: true\n",
        "base: &base {name: worker}\nvalue: {<<: *base, enabled: true}\n",
        "description: |\n  line one\n  line two\n",
    ] {
        assert_eq!(
            parse_manifests(text).unwrap(),
            legacy_documents(text),
            "{text}"
        );
    }
}

#[test]
fn invalid_manifests_fail_instead_of_disappearing_from_the_report() {
    for text in [
        "kind: [\n",
        "kind: Test\nkind: Another\n",
        "kind: *missing\n",
        "kind: Test\n---\nkind: [\n",
    ] {
        assert!(
            serde_yaml::Deserializer::from_str(text)
                .any(|doc| serde_yaml::Value::deserialize(doc).is_err()),
            "legacy parser must also reject {text}"
        );
        assert!(parse_manifests(text).is_err(), "{text}");
    }
}
