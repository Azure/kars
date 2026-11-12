// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::crd::RuntimeSpec;

fn candidate_value(yaml: &str) -> Result<serde_yaml::Value, serde_saphyr::Error> {
    serde_saphyr::from_str_with_options(
        yaml,
        serde_saphyr::options! {
            strict_booleans: true,
            no_schema: true,
            merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
            with_snippet: false,
        },
    )
}

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
    let candidate: RuntimeSpec = serde_saphyr::from_str_with_options(
        yaml,
        serde_saphyr::options! { strict_booleans: true, with_snippet: false },
    )
    .expect("candidate must parse runtime");
    assert_eq!(serde_json::to_value(candidate).unwrap(), json);
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
            serde_saphyr::from_str::<RuntimeSpec>(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn yaml_compatibility_candidate_policy_corpus() {
    for yaml in [
        include_str!("../../cli/profiles/agt/kars-default.yaml"),
        include_str!("../../cli/profiles/agt/kars-offload.yaml"),
        include_str!("../../deploy/helm/kars/files/kars-default-agt-profile.yaml"),
        "version: '1.0'\nagent: &name test\npolicies: [{name: *name}]\n",
        "base: &base {name: worker}\nvalue: {<<: *base, enabled: true}\n",
        "apiVersion: kars.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - host: example.com\n      port: 443\n",
        "yes: yes\nno: no\non: on\noff: off\nboolean: true\n",
        "max: 18446744073709551615\nquoted: '01'\n",
    ] {
        let old: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        let new = candidate_value(yaml).expect("candidate must parse policy corpus");
        assert_eq!(old, new);
        assert_eq!(
            serde_yaml::to_string(&old).unwrap(),
            serde_yaml::to_string(&new).unwrap(),
            "mapping order and scalar types must also agree"
        );
    }
    for invalid in [
        "version: '1.0'\nversion: '2.0'\n",
        "version: '1.0'\n---\nversion: '2.0'\n",
        "agent: *missing\n",
        "policies: [\n",
    ] {
        assert!(serde_yaml::from_str::<serde_yaml::Value>(invalid).is_err());
        assert!(candidate_value(invalid).is_err());
    }
}

#[test]
fn yaml_compatibility_candidate_blocks_runtime_migration() {
    // Both are currently accepted customer inputs. Inferring 01 as a float
    // would reject the host/version after a blind parser substitution.
    let egress = "apiVersion: kars.dev/v1alpha1\nkind: EgressAllowlist\nmetadata:\n  generation: 1\nspec:\n  endpoints:\n    - host: 01\n      port: 443\n";
    let old = crate::policy_canonical::egress::parse(egress.as_bytes()).unwrap();
    assert_eq!(old.endpoints[0].host, "01");
    let new = candidate_value(egress).unwrap();
    assert!(new["spec"]["endpoints"][0]["host"].as_str().is_none());

    let profile = "version: 01\nagent: test\npolicies: []\n";
    let old = crate::policy_canonical::tools::parse(profile.as_bytes()).unwrap();
    assert_eq!(old.version, "01");
    let new = candidate_value(profile).unwrap();
    assert!(new["version"].as_str().is_none());

    let tagged = "value: !custom 42\n";
    let old: serde_yaml::Value = serde_yaml::from_str(tagged).unwrap();
    let new = candidate_value(tagged).unwrap();
    assert!(matches!(old["value"], serde_yaml::Value::Tagged(_)));
    assert!(!matches!(new["value"], serde_yaml::Value::Tagged(_)));
}
