// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde::Deserialize;

fn legacy_documents(text: &str) -> Vec<legacy_yaml::Value> {
    legacy_yaml::Deserializer::from_str(text)
        .map(|doc| legacy_yaml::Value::deserialize(doc).expect("legacy manifest must parse"))
        .filter(|v| !v.is_null())
        .collect()
}

fn assert_matches_legacy(text: &str) {
    let old = legacy_documents(text);
    let new = parse_manifests(text).expect("maintained parser must read manifest");
    assert_eq!(new.len(), old.len(), "{text}");
    for (old, new) in old.iter().zip(&new) {
        assert_eq!(*new, serde_yaml::to_value(old).unwrap(), "{text}");
        assert_eq!(
            serde_yaml::to_string(new).unwrap(),
            legacy_yaml::to_string(old).unwrap(),
            "{text}"
        );
    }
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
            assert_matches_legacy(&text);
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
    assert!(!old.is_empty(), "rendered corpus must contain documents");
    assert_matches_legacy(&text);
}

#[test]
fn yaml_compatibility_streams_and_scalars() {
    for text in [
        "",
        "# comment only\n",
        "---\nkind: Test\n---\nnull\n---\nkind: Another\n",
        "metadata: {name: '01', labels: {enabled: 'true'}}\n",
        "kind: Test\nvalue: 01\ncustom: !custom 42\n",
        "yes: yes\nno: no\non: on\noff: off\nboolean: true\n",
        "base: &base {name: worker}\nvalue: {<<: *base, enabled: true}\n",
        "? [complex, key]\n: value\n",
        "description: |\n  line one\n  line two\n",
    ] {
        assert_matches_legacy(text);
    }
    let docs = parse_manifests("value: 01\ncustom: !custom 42\n").unwrap();
    assert_eq!(docs[0]["value"].as_str(), Some("01"));
    assert!(matches!(docs[0]["custom"], Value::Tagged(_)));
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
            legacy_yaml::Deserializer::from_str(text)
                .any(|doc| legacy_yaml::Value::deserialize(doc).is_err()),
            "legacy parser must also reject {text}"
        );
        assert!(parse_manifests(text).is_err(), "{text}");
    }
}
