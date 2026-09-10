// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

#[path = "../src/cli.rs"]
mod cli;
#[path = "../../controller/src/kars_eval_reconciler/runner.rs"]
mod controller_runner;

use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn controller_job_arguments_match_the_actual_runner_parser() {
    for label in [
        "builtin:jailbreak-baseline",
        "registry.example/corpus@sha256:abc",
    ] {
        let spec = controller_runner::runner_pod_spec_json(
            "evaluation",
            "karseval-evaluation-corpus",
            "ghcr.io/azure/kars/conformance-runner:latest",
            "http://agent.kars-agent.svc.cluster.local:8443",
            label,
        );
        let args = spec["containers"][0]["args"]
            .as_array()
            .expect("runner arguments")
            .iter()
            .map(|arg| arg.as_str().expect("string argument"));
        let cli = cli::Cli::try_parse_from(std::iter::once("kars-conformance-runner").chain(args))
            .expect("the controller must invoke the CLI the runner actually implements");
        assert_eq!(cli.corpus, "/etc/kars/eval-corpus/corpus.json");
        assert_eq!(
            cli.router_base,
            "http://agent.kars-agent.svc.cluster.local:8443"
        );
        assert_eq!(cli.output, PathBuf::from("/dev/stdout"));
        assert_eq!(cli.timeout(), Duration::from_secs(5));
        assert!(cli.forward_proxy.is_none());
        assert!(cli.auth_header.is_none());
        assert!(cli.only_case.is_none());
        assert!(cli.only_tag.is_none());
        assert!(!cli.no_stdout);
    }
}

#[test]
fn legacy_corpus_label_argument_is_not_a_supported_runner_option() {
    let error = cli::Cli::try_parse_from([
        "kars-conformance-runner",
        "--corpus",
        "/etc/kars/eval-corpus/corpus.json",
        "--corpus-label",
        "builtin:jailbreak-baseline",
        "--router-base",
        "http://localhost:8443",
        "--output",
        "/dev/stdout",
    ])
    .expect_err("the historical invocation must reproduce the parser mismatch");
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn controller_job_declares_restricted_settings_matching_the_runner_image() {
    let spec = controller_runner::runner_pod_spec_json(
        "evaluation",
        "karseval-evaluation-corpus",
        "ghcr.io/azure/kars/conformance-runner:latest",
        "http://agent.kars-agent.svc.cluster.local:8443",
        "builtin:jailbreak-baseline",
    );
    assert_eq!(spec["restartPolicy"], "Never");
    assert_eq!(spec["securityContext"]["runAsNonRoot"], true);
    assert!(spec["securityContext"].get("runAsUser").is_none());
    assert_eq!(
        spec["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    let container = &spec["containers"][0];
    assert_eq!(
        container["securityContext"]["allowPrivilegeEscalation"],
        false
    );
    assert_eq!(container["securityContext"]["runAsNonRoot"], true);
    assert!(container["securityContext"].get("runAsUser").is_none());
    assert_eq!(
        container["securityContext"]["capabilities"]["drop"],
        serde_json::json!(["ALL"])
    );
    assert_eq!(
        container["securityContext"]["seccompProfile"]["type"],
        "RuntimeDefault"
    );
    assert_eq!(container["volumeMounts"][0]["readOnly"], true);
    assert_eq!(
        container["volumeMounts"][0]["mountPath"],
        "/etc/kars/eval-corpus"
    );
    assert_eq!(
        spec["volumes"][0]["configMap"]["name"],
        "karseval-evaluation-corpus"
    );
    assert!(
        include_str!("../../sandbox-images/conformance-runner/Dockerfile")
            .lines()
            .any(|line| line.trim() == "USER 1000:1000")
    );
}
