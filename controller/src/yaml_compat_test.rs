// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::crd::RuntimeSpec;
use serde::Deserialize;

#[test]
fn yaml_compatibility_runtime_spec() {
    let yaml = r#"
# Operator-authored runtime configuration.
kind: Hermes
hermes:
  version: "0.5.1"
  agentCode:
    oci:
      image: "registry.example/agent:1.0"
  extraEnv:
    FEATURE: "true"
    VERSION: "01"
"#;
    let spec: RuntimeSpec = serde_yaml::from_str(yaml).expect("runtime YAML must parse");
    let json = serde_json::to_value(spec).expect("runtime must serialize");
    let legacy: RuntimeSpec = legacy_yaml::from_str(yaml).expect("legacy runtime YAML must parse");
    assert_eq!(serde_json::to_value(legacy).unwrap(), json);
    assert_eq!(json["kind"], "Hermes");
    assert_eq!(json["hermes"]["version"], "0.5.1");
    assert_eq!(
        json["hermes"]["agentCode"]["oci"]["image"],
        "registry.example/agent:1.0"
    );
    assert_eq!(json["hermes"]["extraEnv"]["FEATURE"], "true");
    assert_eq!(json["hermes"]["extraEnv"]["VERSION"], "01");

    for invalid in [
        "kind: UnknownRuntime\n",
        "kind: Hermes\nkind: BYO\n",
        "kind: Hermes\n---\nkind: BYO\n",
    ] {
        assert!(
            serde_yaml::from_str::<RuntimeSpec>(invalid).is_err(),
            "{invalid}"
        );
        assert!(
            legacy_yaml::from_str::<RuntimeSpec>(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn yaml_compatibility_fork_values_and_serialized_bytes() {
    let cases = [
        include_str!("../../cli/profiles/agt/kars-default.yaml"),
        include_str!("../../cli/profiles/agt/kars-offload.yaml"),
        include_str!("../../deploy/helm/kars/files/kars-default-agt-profile.yaml"),
        "version: 01\nagent: test\npolicies: []\n",
        "value: !custom 42\nnested: !other {value: yes}\n",
        "base: &base {name: worker}\nvalue: {<<: *base, enabled: true}\n",
        "yes: yes\nno: no\non: on\noff: off\nboolean: true\n",
        "max: 18446744073709551615\nquoted: '01'\nnan: .nan\n",
        "apiVersion: kars.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - host: 01\n      port: 443\n",
    ];
    for input in cases {
        let old: legacy_yaml::Value = legacy_yaml::from_str(input).unwrap();
        let new: serde_yaml::Value = serde_yaml::from_str(input).unwrap();
        assert_eq!(
            legacy_yaml::to_value(&new).unwrap(),
            old,
            "parsed values differ for {input}"
        );
        assert_eq!(
            serde_yaml::to_string(&new).unwrap(),
            legacy_yaml::to_string(&old).unwrap(),
            "serialized bytes differ for {input}"
        );
    }

    let old: legacy_yaml::Value = legacy_yaml::from_str("value: !custom 42\n").unwrap();
    let new: serde_yaml::Value = serde_yaml::from_str("value: !custom 42\n").unwrap();
    assert!(matches!(old["value"], legacy_yaml::Value::Tagged(_)));
    assert!(matches!(new["value"], serde_yaml::Value::Tagged(_)));
    assert_eq!(
        serde_yaml::from_str::<serde_yaml::Value>("value: 01\n").unwrap()["value"].as_str(),
        Some("01")
    );
}

#[test]
fn yaml_compatibility_fork_streams_and_errors() {
    for input in [
        "version: '1.0'\nversion: '2.0'\n",
        "version: '1.0'\n---\nversion: '2.0'\n",
        "agent: *missing\n",
        "policies: [\n",
        "map: {a: 1, a: 2}\n",
    ] {
        let old = legacy_yaml::from_str::<legacy_yaml::Value>(input).unwrap_err();
        let new = serde_yaml::from_str::<serde_yaml::Value>(input).unwrap_err();
        assert_eq!(old.to_string(), new.to_string(), "{input}");
        assert_eq!(
            old.location().map(|l| (l.line(), l.column())),
            new.location().map(|l| (l.line(), l.column())),
            "{input}"
        );
    }
    let stream = "---\na: 01\n---\nb: !custom test\n---\nnull\n";
    let old: Vec<_> = legacy_yaml::Deserializer::from_str(stream)
        .map(|doc| legacy_yaml::Value::deserialize(doc).unwrap())
        .collect();
    let new: Vec<_> = serde_yaml::Deserializer::from_str(stream)
        .map(|doc| serde_yaml::Value::deserialize(doc).unwrap())
        .collect();
    assert_eq!(old.len(), new.len());
    for (old, new) in old.iter().zip(new.iter()) {
        assert_eq!(*old, legacy_yaml::to_value(new).unwrap());
        assert_eq!(
            legacy_yaml::to_string(old).unwrap(),
            serde_yaml::to_string(new).unwrap()
        );
    }

    let invalid_utf8 = b"agent: \xff\n";
    let old = legacy_yaml::from_slice::<legacy_yaml::Value>(invalid_utf8).unwrap_err();
    let new = serde_yaml::from_slice::<serde_yaml::Value>(invalid_utf8).unwrap_err();
    assert_eq!(old.to_string(), new.to_string());
    assert_eq!(
        old.location().map(|l| (l.line(), l.column())),
        new.location().map(|l| (l.line(), l.column()))
    );
}

#[test]
fn yaml_compatibility_fork_reader_error_source_change() {
    use std::error::Error as _;

    struct FailingReader;
    impl std::io::Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("reader failure"))
        }
    }

    let old = legacy_yaml::from_reader::<_, legacy_yaml::Value>(FailingReader).unwrap_err();
    let new = serde_yaml::from_reader::<_, serde_yaml::Value>(FailingReader).unwrap_err();
    assert_eq!(old.to_string(), new.to_string());
    assert!(old.source().is_none());
    assert!(new.source().is_some());
}

#[test]
fn yaml_compatibility_fork_signed_policy_bytes() {
    let egress = "apiVersion: kars.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - host: 01\n      port: 53\n    - host: example.com\n      port: 443\n";
    let old: legacy_yaml::Value = legacy_yaml::from_str(egress).unwrap();
    let legacy_endpoints: Vec<_> = old["spec"]["endpoints"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|endpoint| crate::crd::EndpointConfig {
            host: endpoint["host"].as_str().unwrap().to_string(),
            port: Some(endpoint["port"].as_u64().unwrap() as u16),
        })
        .collect();
    let parsed = crate::policy_canonical::egress::parse(egress.as_bytes()).unwrap();
    let fork_endpoints: Vec<_> = parsed
        .endpoints
        .iter()
        .map(|endpoint| crate::crd::EndpointConfig {
            host: endpoint.host.clone(),
            port: Some(endpoint.port),
        })
        .collect();
    let old_body = serde_json::to_vec(&crate::egress_allowlist_compile::compile_to_doc(
        &legacy_endpoints,
    ))
    .unwrap();
    let new_body = serde_json::to_vec(&crate::egress_allowlist_compile::compile_to_doc(
        &fork_endpoints,
    ))
    .unwrap();
    assert_eq!(old_body, new_body);
    assert_eq!(
        new_body,
        br#"{"schemaVersion":1,"endpoints":[{"host":"01","port":53},{"host":"example.com","port":443}]}"#
    );
    assert_eq!(
        crate::egress_allowlist_compile::egress_allowlist_digest(&old_body),
        crate::egress_allowlist_compile::egress_allowlist_digest(&new_body)
    );

    for profile in [
        "version: 01\nagent: &name test\npolicies: [{name: *name}]\n",
        include_str!("../../cli/profiles/agt/kars-default.yaml"),
        include_str!("../../cli/profiles/agt/kars-offload.yaml"),
    ] {
        let legacy: legacy_yaml::Value = legacy_yaml::from_str(profile).unwrap();
        let verified = crate::policy_canonical::tools::parse(profile.as_bytes()).unwrap();
        assert_eq!(verified.version, legacy["version"].as_str().unwrap());
        assert_eq!(verified.agent, legacy["agent"].as_str().unwrap());
        assert_eq!(
            verified.policies_count,
            legacy["policies"].as_sequence().unwrap().len()
        );
        assert_eq!(verified.bytes, profile.as_bytes());
        assert_eq!(
            crate::tool_policy_compile::agt_profile_digest(profile),
            crate::tool_policy_compile::agt_profile_digest(
                std::str::from_utf8(&verified.bytes).unwrap()
            )
        );
    }
}
