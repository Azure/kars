// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::crd::RuntimeSpec;

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
    }
}
