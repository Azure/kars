// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const JOB_PATH: &str = "/apis/batch/v1/namespaces/kars-demo/cronjobs/demo-blocklist-refresh";
const NS_PATH: &str = "/api/v1/namespaces/kars-demo";
const LEGACY_SCRIPT: &str = concat!(
    "set -e; ",
    "echo '# Auto-refreshed blocklist' > /tmp/domains.txt; ",
    "echo '# Updated: '$(date -u +%Y-%m-%dT%H:%M:%SZ) >> /tmp/domains.txt; ",
    "curl -sfL --max-time 60 --max-filesize 52428800 ",
    "'https://small.oisd.nl/domainswild' >> /tmp/domains.txt 2>/dev/null || ",
    "echo '# OISD fetch failed' >> /tmp/domains.txt; ",
    "curl -sfL --max-time 60 --max-filesize 52428800 ",
    "'https://urlhaus.abuse.ch/downloads/hostfile/' >> /tmp/urlhaus.txt 2>/dev/null && ",
    "grep '^127\\.0\\.0\\.1' /tmp/urlhaus.txt | awk '{print $2}' >> /tmp/domains.txt || ",
    "echo '# URLhaus fetch failed' >> /tmp/domains.txt; ",
    "kubectl create configmap ",
);

fn legacy_job() -> Value {
    json!({
        "apiVersion": "batch/v1", "kind": "CronJob",
        "metadata": {
            "name": "demo-blocklist-refresh", "namespace": "kars-demo",
            "uid": "job-a", "resourceVersion": "30",
            "labels": {"kars.azure.com/sandbox": "demo", "kars.azure.com/component": "blocklist-refresh"},
            "managedFields": [{
                "manager": CLAWSANDBOX, "operation": "Apply", "apiVersion": "batch/v1",
                "fieldsType": "FieldsV1", "fieldsV1": {
                    "f:metadata": {"f:labels": {"f:kars.azure.com/sandbox": {}, "f:kars.azure.com/component": {}}},
                    "f:spec": {"f:jobTemplate": {"f:spec": {"f:template": {"f:spec": {
                        "f:containers": {"k:{\"name\":\"refresh\"}": {"f:image": {}, "f:command": {}, "f:args": {}}}
                    }}}}}
                }
            }]
        },
        "spec": {
            "schedule": "0 */6 * * *", "successfulJobsHistoryLimit": 1, "failedJobsHistoryLimit": 1,
            "jobTemplate": {"spec": {"backoffLimit": 2, "template": {"spec": {
                "restartPolicy": "OnFailure", "serviceAccountName": "sandbox",
                "containers": [{"name": "refresh", "image": "mcr.microsoft.com/cbl-mariner/base/core:2.0",
                    "command": ["sh", "-c", LEGACY_SCRIPT],
                    "args": ["demo-blocklist --from-file=domains.txt=/tmp/domains.txt -n kars-demo --dry-run=client -o yaml | kubectl apply -f -"],
                    "resources": {"requests": {"cpu": "50m", "memory": "64Mi"},
                        "limits": {"cpu": "200m", "memory": "256Mi"}},
                    "securityContext": {"runAsNonRoot": true, "runAsUser": 65534,
                        "readOnlyRootFilesystem": true, "allowPrivilegeEscalation": false},
                    "volumeMounts": [{"name": "scratch", "mountPath": "/tmp"}]
                }],
                "volumes": [{"name": "scratch", "emptyDir": {"sizeLimit": "128Mi"}}]
            }}}}
        }
    })
}

fn params(value: Value) -> Result<DeleteParams, ReconcileError> {
    deletion_params(&serde_json::from_value(value).unwrap(), "demo", "kars-demo")
}

fn original_legacy_job() -> Value {
    let mut job = legacy_job();
    let pod = &mut job["spec"]["jobTemplate"]["spec"]["template"]["spec"];
    pod.as_object_mut().unwrap().remove("volumes");
    let container = &mut pod["containers"][0];
    container["securityContext"]["readOnlyRootFilesystem"] = json!(false);
    container.as_object_mut().unwrap().remove("volumeMounts");
    job
}

fn with_kubernetes_defaults(mut job: Value) -> Value {
    let pod = &mut job["spec"]["jobTemplate"]["spec"]["template"]["spec"];
    for (key, value) in [
        ("dnsPolicy", json!("ClusterFirst")),
        ("schedulerName", json!("default-scheduler")),
        ("terminationGracePeriodSeconds", json!(30)),
        ("securityContext", json!({})),
        ("enableServiceLinks", json!(true)),
        ("preemptionPolicy", json!("PreemptLowerPriority")),
        ("serviceAccount", json!("sandbox")),
    ] {
        pod[key] = value;
    }
    let container = &mut pod["containers"][0];
    container["imagePullPolicy"] = json!("IfNotPresent");
    container["terminationMessagePath"] = json!("/dev/termination-log");
    container["terminationMessagePolicy"] = json!("File");
    job
}

#[test]
fn recognized_helper_has_uid_and_version_fenced_foreground_cleanup() {
    for job in [
        legacy_job(),
        original_legacy_job(),
        with_kubernetes_defaults(legacy_job()),
        with_kubernetes_defaults(original_legacy_job()),
    ] {
        let params = serde_json::to_value(params(job).unwrap()).unwrap();
        assert_eq!(params["propagationPolicy"], "Foreground");
        assert_eq!(
            params["preconditions"],
            json!({"uid": "job-a", "resourceVersion": "30"})
        );
    }
}

#[test]
fn rejects_missing_identity_ownership_and_controller_field_evidence() {
    for pointer in [
        "/metadata/uid",
        "/metadata/resourceVersion",
        "/metadata/labels",
        "/metadata/managedFields",
        "/metadata/managedFields/0/fieldsV1/f:metadata",
        "/metadata/managedFields/0/fieldsV1/f:spec",
    ] {
        let mut job = legacy_job();
        *job.pointer_mut(pointer).unwrap() = Value::Null;
        assert!(params(job).is_err(), "{pointer}");
    }
    for (pointer, replacement) in [
        ("/metadata/name", json!("foreign")),
        ("/metadata/namespace", json!("kars-foreign")),
        ("/metadata/uid", json!("")),
        ("/metadata/resourceVersion", json!("")),
        ("/metadata/labels/kars.azure.com~1sandbox", json!("foreign")),
        (
            "/metadata/labels/kars.azure.com~1component",
            json!("foreign"),
        ),
        ("/metadata/managedFields/0/manager", json!("foreign")),
        ("/metadata/managedFields/0/operation", json!("Update")),
    ] {
        let mut job = legacy_job();
        *job.pointer_mut(pointer).unwrap() = replacement;
        assert!(params(job).is_err(), "{pointer}");
    }
    let mut job = legacy_job();
    job["metadata"]["ownerReferences"] = json!([{
        "apiVersion": "v1", "kind": "Namespace", "name": "foreign", "uid": "foreign"
    }]);
    assert!(params(job).is_err());
}

#[test]
fn rejects_customized_workloads_and_extra_containers() {
    for (key, replacement) in [
        ("name", json!("foreign")),
        ("image", json!("example.invalid/custom:latest")),
        ("command", json!(["sh", "-c", "custom command"])),
        (
            "command",
            json!(["sh", "-c", format!("custom command; {LEGACY_SCRIPT}")]),
        ),
        ("command", json!(["sh", "-c"])),
        (
            "command",
            json!(["sh", "-c", LEGACY_SCRIPT, "unexpected-arg"]),
        ),
        ("command", json!(["bash", "-c", LEGACY_SCRIPT])),
        ("args", json!(["foreign-configmap"])),
        ("args", json!([])),
        (
            "env",
            json!([{"name": "CUSTOM_SETTING", "value": "custom"}]),
        ),
        (
            "envFrom",
            json!([{"secretRef": {"name": "custom-credentials"}}]),
        ),
        (
            "lifecycle",
            json!({"postStart": {"exec": {"command": ["custom-task"]}}}),
        ),
        (
            "livenessProbe",
            json!({"exec": {"command": ["custom-task"]}}),
        ),
        ("workingDir", json!("/custom")),
        ("resources", json!({"requests": {"cpu": "100m"}})),
        (
            "volumeMounts",
            json!([{"name": "custom", "mountPath": "/tmp"}]),
        ),
        ("imagePullPolicy", json!("Always")),
        (
            "securityContext",
            json!({"runAsUser": 0, "readOnlyRootFilesystem": true}),
        ),
        ("terminationMessagePath", json!("/custom")),
    ] {
        let mut job = legacy_job();
        job["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0][key] = replacement;
        assert!(params(job).is_err(), "{key}");
    }
    for key in ["containers", "initContainers", "ephemeralContainers"] {
        let mut job = legacy_job();
        let pod = &mut job["spec"]["jobTemplate"]["spec"]["template"]["spec"];
        if key == "containers" {
            pod[key]
                .as_array_mut()
                .unwrap()
                .push(json!({"name": "foreign"}));
        } else {
            pod[key] = json!([{"name": "foreign"}]);
        }
        assert!(params(job).is_err(), "{key}");
    }
    for (key, replacement) in [
        ("serviceAccountName", json!("foreign")),
        ("restartPolicy", json!("Never")),
        (
            "volumes",
            json!([{"name": "scratch", "secret": {"secretName": "custom"}}]),
        ),
        ("volumes", json!([])),
        ("runtimeClassName", json!("custom")),
        ("nodeSelector", json!({"custom": "true"})),
        ("dnsPolicy", json!("Default")),
        ("securityContext", json!({"runAsUser": 0})),
        ("terminationGracePeriodSeconds", json!(60)),
        ("serviceAccount", json!("foreign")),
    ] {
        let mut job = legacy_job();
        job["spec"]["jobTemplate"]["spec"]["template"]["spec"][key] = replacement;
        assert!(params(job).is_err(), "{key}");
    }
    for pointer in ["/spec/jobTemplate", "/spec/jobTemplate/spec/template"] {
        let mut job = legacy_job();
        job.pointer_mut(pointer).unwrap()["metadata"] =
            json!({"annotations": {"custom-workload": "enabled"}});
        assert!(params(job).is_err(), "{pointer}");
    }
}

#[test]
fn rejects_fields_reassigned_to_another_manager() {
    for key in ["f:image", "f:command", "f:args"] {
        let mut job = legacy_job();
        let fields = &mut job["metadata"]["managedFields"][0]["fieldsV1"];
        fields["f:spec"]["f:jobTemplate"]["f:spec"]["f:template"]["f:spec"]["f:containers"]
            ["k:{\"name\":\"refresh\"}"].as_object_mut().unwrap().remove(key);
        assert!(params(job).is_err(), "{key}");
    }
}

struct NamespaceReads {
    original: Value,
    later: Value,
    count: Arc<AtomicUsize>,
}

impl Respond for NamespaceReads {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let body = if self.count.fetch_add(1, Ordering::SeqCst) == 0 {
            &self.original
        } else {
            &self.later
        };
        ResponseTemplate::new(200).set_body_json(body)
    }
}

fn api_error(code: u16) -> Value {
    json!({"apiVersion": "v1", "kind": "Status", "status": "Failure",
        "message": "test API rejection", "reason": "TestError", "code": code})
}

async fn run_retire(
    job: Option<Value>,
    namespace_changes: bool,
    delete_code: u16,
) -> (Result<(), ReconcileError>, Vec<Request>) {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/compat/fixtures/namespace-legacy.json"
    ))
    .unwrap();
    let sandbox: KarsSandbox = serde_json::from_value(fixture["sandbox"].clone()).unwrap();
    let mut namespace = fixture["namespace"].clone();
    namespace["metadata"]["annotations"] = json!({
        "kars.azure.com/namespace-claim-version": "v1",
        "kars.azure.com/sandbox-namespace": "workspace-a",
        "kars.azure.com/sandbox-name": "demo",
        "kars.azure.com/sandbox-uid": "sandbox-a"
    });
    let expected: Namespace = serde_json::from_value(namespace.clone()).unwrap();
    let mut later = namespace.clone();
    if namespace_changes {
        later["metadata"]["uid"] = json!("replacement-namespace");
    }
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(NS_PATH))
        .respond_with(NamespaceReads {
            original: namespace,
            later,
            count: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(JOB_PATH))
        .respond_with(match job {
            Some(job) => ResponseTemplate::new(200).set_body_json(job),
            None => ResponseTemplate::new(404).set_body_json(api_error(404)),
        })
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::method("DELETE"))
        .and(wiremock::matchers::path(JOB_PATH))
        .respond_with(
            ResponseTemplate::new(delete_code).set_body_json(if delete_code < 300 {
                legacy_job()
            } else {
                api_error(delete_code)
            }),
        )
        .mount(&server)
        .await;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let result = retire(&client, &sandbox, &expected).await;
    (result, server.received_requests().await.unwrap())
}

#[tokio::test]
async fn absent_helper_does_not_create_or_mutate_resources() {
    let (result, requests) = run_retire(None, false, 202).await;
    result.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test]
async fn retirement_rechecks_namespace_then_deletes_only_the_observed_helper() {
    let (result, requests) = run_retire(Some(legacy_job()), false, 202).await;
    result.unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].url.path(), NS_PATH);
    assert_eq!(requests[1].url.path(), JOB_PATH);
    assert_eq!(requests[2].url.path(), NS_PATH);
    assert_eq!(requests[3].method, "DELETE");
    assert_eq!(requests[3].url.path(), JOB_PATH);
    let body: Value = serde_json::from_slice(&requests[3].body).unwrap();
    assert_eq!(body["propagationPolicy"], "Foreground");
    assert_eq!(
        body["preconditions"],
        json!({"uid": "job-a", "resourceVersion": "30"})
    );
}

#[tokio::test]
async fn ownership_conflicts_never_delete_resources() {
    let (result, requests) = run_retire(Some(legacy_job()), true, 202).await;
    assert!(result.is_err());
    assert!(requests.iter().all(|request| request.method == "GET"));
    let mut unowned = legacy_job();
    unowned["metadata"]["managedFields"] = Value::Null;
    let mut customized = legacy_job();
    customized["spec"]["jobTemplate"]["spec"]["template"]["spec"]["containers"][0]["lifecycle"] =
        json!({"postStart": {"exec": {"command": ["custom-task"]}}});
    for job in [unowned, customized] {
        let (result, requests) = run_retire(Some(job), false, 202).await;
        assert!(result.is_err());
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.method == "GET"));
    }
}

#[tokio::test]
async fn concurrent_removal_is_idempotent_but_delete_failures_are_not_hidden() {
    let (result, _) = run_retire(Some(legacy_job()), false, 404).await;
    result.unwrap();
    for code in [403, 409, 500] {
        let (result, requests) = run_retire(Some(legacy_job()), false, code).await;
        assert!(result.is_err(), "{code}");
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.method == "DELETE")
                .count(),
            1
        );
    }
}
