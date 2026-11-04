// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::collections::BTreeMap;

#[test]
fn scoped_annotation_pair_is_required_and_cannot_be_downgraded() {
    let scope = format!("{}:0", "a".repeat(32));
    let mut annotations = BTreeMap::from([
        (REQUEST_SCOPE.into(), scope.clone()),
        (REQUEST_ID.into(), format!("{scope}:1")),
    ]);
    assert!(RequestBinding::from_annotations(None).unwrap().is_none());
    assert!(
        RequestBinding::from_annotations(Some(&annotations))
            .unwrap()
            .is_some()
    );
    for request in [
        "different:1".into(),
        format!("{scope}:0"),
        format!("{scope}:18446744073709551616"),
    ] {
        annotations.insert(REQUEST_ID.into(), request);
        assert!(RequestBinding::from_annotations(Some(&annotations)).is_err());
    }
    annotations.remove(REQUEST_ID);
    assert!(RequestBinding::from_annotations(Some(&annotations)).is_err());
    annotations = BTreeMap::from([(REQUEST_ID.into(), format!("{scope}:1"))]);
    assert!(RequestBinding::from_annotations(Some(&annotations)).is_err());
}

#[test]
fn scoped_file_has_exact_endpoint_and_no_legacy_host_authority() {
    let scope = format!("{}:0", "a".repeat(32));
    let binding = RequestBinding {
        request: format!("{scope}:1"),
        scope,
    };
    let original = compile_approval_file(
        "grant",
        "sandbox",
        &[EndpointConfig {
            host: "approved.example".into(),
            port: Some(443),
        }],
        "human approved",
        None,
        "2026-10-22T00:00:00Z",
        "2026-10-22T00:15:00Z",
    );
    let scoped = binding.compile(original.clone()).unwrap();
    assert_eq!(original["schemaVersion"], 1);
    assert_eq!(scoped["schemaVersion"], 2);
    assert_eq!(scoped["requestScope"], binding.scope);
    assert_eq!(scoped["requestId"], binding.request);
    assert_eq!(scoped["scopedHosts"], original["hosts"]);
    assert!(scoped.get("hosts").is_none());
    assert_eq!(scoped["expiresAt"], original["expiresAt"]);
    assert!(binding.compile(json!({"hosts":[]})).is_err());
    assert!(binding.compile(json!({"hosts":[{},{}]})).is_err());
}
