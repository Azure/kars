// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TASK: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/run";
const SANDBOX: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/run";
const NS: &str = "/api/v1/namespaces/kars-run";
const HELPER: &str = "/apis/apps/v1/namespaces/core/deployments/dispatcher";
const DEPLOYMENT: &str = "/apis/apps/v1/namespaces/kars-run/deployments/run";
const ROOT: &str = "/api/v1/namespaces/kars-run/secrets/kars-mission-runtime-identity";
const HELPER_ROOT: &str = "/api/v1/namespaces/core/secrets/kars-mission-dispatcher-identity";
const BINDING: &str = "/api/v1/namespaces/work/configmaps/kars-mission-binding-run";
const RECOVERY: &str = "kars.azure.com/mission-root-recovery-uid";

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Value>,
    calls: Vec<(String, String, bool, Value)>,
    fail: Option<(String, String, u16, bool)>,
    change_on_get: Option<(String, usize)>,
    truncate: Option<String>,
}
struct Fixture {
    _server: MockServer,
    client: Client,
    state: Arc<Mutex<State>>,
}
fn merge(target: &mut Value, patch: &Value) {
    if let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            if value.is_null() {
                target.remove(key);
            } else {
                merge(target.entry(key).or_insert(Value::Null), value);
            }
        }
    } else {
        *target = patch.clone();
    }
}
fn status(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({"apiVersion":"v1","kind":"Status",
        "status":"Failure","code":code,"reason":if code==404 {"NotFound"} else {"Conflict"},
        "message":"sensitive upstream detail must not escape"}))
}
fn reply(value: &Value, metadata: bool) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(if metadata {
        json!({"apiVersion":"meta.k8s.io/v1","kind":"PartialObjectMetadata","metadata":value["metadata"]})
    } else { value.clone() })
}
fn config() -> Config {
    Config {
        namespace: "core".into(),
        deployment: "dispatcher".into(),
        release: "kars".into(),
    }
}
fn workload_objects(namespace: &str, name: &str, container: &str) -> (Value, Value, Value) {
    let template = json!({"metadata":{"labels":{"app":name}},"spec":{"containers":[{"name":container,"image":"test:latest"}]}});
    let mut deployment = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{
        "name":name,"namespace":namespace,"uid":format!("{name}-deployment"),"resourceVersion":"1","generation":1,
        "annotations":{"deployment.kubernetes.io/revision":"1"}},
        "spec":{"replicas":1,"selector":{"matchLabels":{"app":name}},"strategy":{"type":"Recreate"},"template":template},
        "status":{"observedGeneration":1,"replicas":1,"readyReplicas":1,"availableReplicas":1,"updatedReplicas":1}});
    if name == "run" {
        deployment["metadata"]["labels"] =
            json!({"kars.azure.com/sandbox":"run","kars.azure.com/component":"sandbox"});
        merge(
            &mut deployment["metadata"]["annotations"],
            &json!({"kars.azure.com/credential-sandbox-uid":"sandbox-uid",
            "kars.azure.com/credential-namespace-uid":"runtime-uid"}),
        );
    } else {
        deployment["metadata"]["labels"] = json!({"app.kubernetes.io/managed-by":"Helm","app.kubernetes.io/component":"mission-dispatcher"});
        merge(
            &mut deployment["metadata"]["annotations"],
            &json!({"meta.helm.sh/release-name":"kars","meta.helm.sh/release-namespace":"core"}),
        );
        deployment["spec"]["template"]["spec"]["containers"][0]["env"] = json!([{"name":"KARS_MISSION_IDENTITY_ROOT","valueFrom":{
            "secretKeyRef":{"name":roots::DISPATCHER_SECRET,"key":"root","optional":false}}}]);
    }
    let (rs, pod) = descendants(&deployment);
    (deployment, rs, pod)
}
fn descendants(deployment: &Value) -> (Value, Value) {
    let name = deployment["metadata"]["name"].as_str().unwrap();
    let ns = &deployment["metadata"]["namespace"];
    let mut template = deployment["spec"]["template"].clone();
    template["metadata"]["labels"]["pod-template-hash"] = "hash".into();
    let rs = json!({"apiVersion":"apps/v1","kind":"ReplicaSet","metadata":{
        "name":format!("{name}-hash"),"namespace":ns,"uid":format!("{name}-rs"),"resourceVersion":"1","generation":1,
        "labels":{"pod-template-hash":"hash"},"annotations":{"deployment.kubernetes.io/revision":"1"},
        "ownerReferences":[{"apiVersion":"apps/v1","kind":"Deployment","name":name,"uid":deployment["metadata"]["uid"],"controller":true}]},
        "spec":{"replicas":1,"selector":{"matchLabels":{"app":name,"pod-template-hash":"hash"}},"template":template},
        "status":{"observedGeneration":1,"replicas":1,"readyReplicas":1,"availableReplicas":1}});
    let pod = json!({"apiVersion":"v1","kind":"Pod","metadata":{
        "name":format!("{name}-hash-pod"),"namespace":ns,"uid":format!("{name}-pod"),"resourceVersion":"1",
        "labels":{"app":name,"pod-template-hash":"hash"},
        "ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":rs["metadata"]["name"],"uid":rs["metadata"]["uid"],"controller":true}]},
        "spec":template["spec"],"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}});
    (rs, pod)
}
impl Fixture {
    async fn new() -> Self {
        let mut task: KarsTask = serde_json::from_value(json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
            "metadata":{"name":"run","namespace":"work","uid":"task-uid","resourceVersion":"1","generation":1,
                "annotations":{"kars.azure.com/run-requested":"preserved-run"}},
            "spec":{"objective":"Write a useful briefing","envelope":{"tier":1,"authorityCeiling":1,"delegationDepth":1},
                "execution":{"launch":true},"blueprint":{"model":{"provider":"azure-openai","deployment":"test"}}}})).unwrap();
        task.status = Some(serde_json::from_value(json!({"phase":"Ready","observedGeneration":1,"executionPhase":"Running",
            "sandboxRef":{"name":"run"},"envelopeDigest":task.envelope_digest(),"lineage":[],"conditions":[{
            "type":"Ready","status":"True","reason":"Reconciled","message":"validated","lastTransitionTime":"2026-01-01T00:00:00Z"}]})).unwrap());
        assert!(crate::kars_task_reconciler::task_is_ready(&task));
        let state = Arc::new(Mutex::new(State::default()));
        {
            let mut s = state.lock().unwrap();
            s.objects
                .insert(TASK.into(), serde_json::to_value(task).unwrap());
            s.objects.insert(SANDBOX.into(), json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
                "metadata":{"name":"run","namespace":"work","uid":"sandbox-uid","resourceVersion":"1","generation":1,
                    "annotations":{"kars.azure.com/namespace-uid":"runtime-uid"},
                    "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"run","uid":"task-uid","controller":true}]},
                "spec":{"runtime":{"kind":"OpenClaw","openclaw":{}},"inferenceRef":{"name":"run-inference"}},"status":{"phase":"Running"}}));
            s.objects.insert(NS.into(), json!({"apiVersion":"v1","kind":"Namespace","metadata":{
                "name":"kars-run","uid":"runtime-uid","resourceVersion":"1","annotations":{
                "kars.azure.com/namespace-claim-version":"v1","kars.azure.com/sandbox-namespace":"work",
                "kars.azure.com/sandbox-name":"run","kars.azure.com/sandbox-uid":"sandbox-uid"}},"status":{"phase":"Active"}}));
            s.objects.insert("/api/v1/namespaces/core".into(), json!({"apiVersion":"v1","kind":"Namespace",
                "metadata":{"name":"core","uid":"core-uid","resourceVersion":"1"},"status":{"phase":"Active"}}));
        }
        let server = MockServer::start().await;
        let captured = state.clone();
        Mock::given(|_: &wiremock::Request| true).respond_with(move |r: &wiremock::Request| {
            let mut s = captured.lock().unwrap();
            let path = r.url.path();
            let method = r.method.as_str();
            let metadata = r.headers.get("accept").and_then(|h| h.to_str().ok()).is_some_and(|h| h.contains("PartialObjectMetadata"));
            let body: Value = r.body_json().unwrap_or(Value::Null);
            s.calls.push((method.into(),path.into(),metadata,body.clone()));
            let failure = s.fail.as_ref().filter(|(m,p,_,_)| m==method && p==path).cloned();
            if let Some((_,_,code,false)) = failure { s.fail = None; return status(code); }
            if method=="GET" {
                if let Some((target, remaining)) = &mut s.change_on_get
                    && target == path {
                    *remaining -= 1;
                    if *remaining == 0 {
                        s.objects.get_mut(path).unwrap()["metadata"]["resourceVersion"] = "999".into();
                        s.change_on_get = None;
                    }
                }
                if let Some(value) = s.objects.get(path) { return reply(value,metadata); }
                for (resource,kind,api) in [("replicasets","ReplicaSet","apps/v1"),("pods","Pod","v1")] {
                    if path.ends_with(&format!("/{resource}")) {
                        let items: Vec<_> = s.objects.iter().filter(|(p,_)| p.starts_with(&format!("{path}/"))).map(|(_,v)|v).collect();
                        return ResponseTemplate::new(200).set_body_json(json!({"apiVersion":api,"kind":format!("{kind}List"),
                            "metadata":{"continue":if s.truncate.as_deref()==Some(path) {"next"}else{""}},"items":items}));
                    }
                }
            }
            if ["POST","PATCH","PUT"].contains(&method) {
                let key = if method=="POST" {format!("{path}/{}",body["metadata"]["name"].as_str().unwrap())} else {path.into()};
                let prior = s.objects.get(&key).cloned();
                if method=="POST" && prior.is_some() { return status(409); }
                if method!="POST" && prior.is_none() { return status(404); }
                if let Some(prior) = &prior
                    && (body["metadata"]["uid"] != prior["metadata"]["uid"] || body["metadata"]["resourceVersion"] != prior["metadata"]["resourceVersion"]) {
                    return status(409);
                }
                let mut value = if method=="PATCH" { prior.clone().unwrap() } else {body.clone()};
                if method=="PATCH" { merge(&mut value,&body); }
                let rv = prior.as_ref().and_then(|p|p["metadata"]["resourceVersion"].as_str()).unwrap_or("0").parse::<u64>().unwrap()+1;
                value["metadata"]["resourceVersion"] = rv.to_string().into();
                if method=="POST" { value["metadata"]["uid"] = format!("created-{}",value["metadata"]["name"].as_str().unwrap()).into(); }
                s.objects.insert(key,value.clone());
                if let Some((_,_,code,true)) = failure { s.fail = None; return status(code); }
                return reply(&value,metadata);
            }
            status(404)
        }).mount(&server).await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        let f = Self {
            _server: server,
            client,
            state,
        };
        for (ns, name, container) in [
            ("core", "dispatcher", "mission-dispatcher"),
            ("kars-run", "run", "openclaw"),
        ] {
            let (d, _, _) = workload_objects(ns, name, container);
            f.install_workload(d);
        }
        f
    }
    fn get<T: DeserializeOwned>(&self, path: &str) -> T {
        serde_json::from_value(self.state.lock().unwrap().objects[path].clone()).unwrap()
    }
    fn set<T: Serialize>(&self, path: &str, value: &T) {
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(path.into(), serde_json::to_value(value).unwrap());
    }
    fn mutate(&self, path: &str, change: impl FnOnce(&mut Value)) {
        change(self.state.lock().unwrap().objects.get_mut(path).unwrap());
    }
    fn install_workload(&self, deployment: Value) {
        let ns = deployment["metadata"]["namespace"].as_str().unwrap();
        let name = deployment["metadata"]["name"].as_str().unwrap();
        let (rs, pod) = descendants(&deployment);
        self.set(
            &format!("/apis/apps/v1/namespaces/{ns}/deployments/{name}"),
            &deployment,
        );
        self.set(
            &format!("/apis/apps/v1/namespaces/{ns}/replicasets/{name}-hash"),
            &rs,
        );
        self.set(
            &format!("/api/v1/namespaces/{ns}/pods/{name}-hash-pod"),
            &pod,
        );
    }
    async fn prepare(&self) -> (Prepared, Deployment) {
        let mut desired = self.get(DEPLOYMENT);
        let prepared = prepare_with(&self.client, &self.get(SANDBOX), &mut desired, &config())
            .await
            .unwrap()
            .unwrap();
        (prepared, desired)
    }
    async fn applied(&self) -> Prepared {
        let (prepared, desired) = self.prepare().await;
        self.install_workload(serde_json::to_value(desired).unwrap());
        prepared
    }
    fn writes(&self, path: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, p, _, _)| m != "GET" && p.starts_with(path))
            .count()
    }
    fn body_reads(&self, path: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|(m, p, meta, _)| m == "GET" && p == path && !meta)
            .count()
    }
}

#[test]
fn configuration_is_default_off_and_validates_explicit_core_custody() {
    for gate in [None, Some("false")] {
        assert!(
            Config::read(|key| {
                assert_eq!(key, "KARS_MISSION_DISPATCH_ENABLED");
                gate.map(str::to_string)
            })
            .unwrap()
            .is_none()
        );
    }
    for gate in ["TRUE", "1", ""] {
        assert!(Config::read(|_| Some(gate.into())).is_err());
    }
    for (key, value) in [
        ("POD_NAMESPACE", ""),
        ("KARS_MISSION_DISPATCHER_DEPLOYMENT", "../escape"),
        ("KARS_MISSION_DISPATCHER_RELEASE", "Upper"),
    ] {
        assert!(
            Config::read(|k| Some(
                if k == key {
                    value
                } else if k == "KARS_MISSION_DISPATCH_ENABLED" {
                    "true"
                } else {
                    "valid"
                }
                .into()
            ))
            .is_err()
        );
    }
    let (d, _, _) = workload_objects("core", "dispatcher", "mission-dispatcher");
    config()
        .validate_dispatcher(&serde_json::from_value(d.clone()).unwrap())
        .unwrap();
    for (path, value) in [
        (
            "/metadata/annotations/meta.helm.sh~1release-name",
            json!("foreign"),
        ),
        ("/metadata/uid", json!("")),
        ("/spec/replicas", json!(2)),
        ("/spec/strategy/type", json!("RollingUpdate")),
        (
            "/spec/template/spec/containers/0/env/0/valueFrom/secretKeyRef/optional",
            json!(true),
        ),
        (
            "/spec/template/spec/containers/0/env/0/valueFrom/secretKeyRef/name",
            json!("another-root"),
        ),
    ] {
        let mut changed = d.clone();
        *changed.pointer_mut(path).unwrap() = value;
        assert!(
            config()
                .validate_dispatcher(&serde_json::from_value(changed).unwrap())
                .is_err(),
            "{path}"
        );
    }
    for extra in [true, false] {
        let mut changed = d.clone();
        if extra {
            changed["spec"]["template"]["spec"]["containers"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"extra","image":"test:latest"}));
        } else {
            changed["spec"]["template"]["spec"]["containers"][0]["env"][0]["valueFrom"]["fieldRef"] =
                json!({"fieldPath":"metadata.uid"});
        }
        assert!(
            config()
                .validate_dispatcher(&serde_json::from_value(changed).unwrap())
                .is_err()
        );
    }
}

#[tokio::test]
async fn prepares_secret_backed_projection_without_mutating_the_requested_run() {
    let f = Fixture::new().await;
    let original: Value = f.get(TASK);
    let (prepared, desired) = f.prepare().await;
    assert_eq!(f.get::<Value>(TASK), original);
    assert_eq!(f.writes(TASK), 0);
    let serialized = serde_json::to_value(&desired).unwrap();
    assert!(
        !serialized
            .to_string()
            .contains(&prepared.runtime_root.value)
    );
    let env = &serialized["spec"]["template"]["spec"]["containers"][0]["env"];
    let find = |name| {
        env.as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .unwrap()
    };
    assert_eq!(find("KARS_MISSION_CONTRACT")["value"], r#"{"version":1}"#);
    assert_eq!(
        serde_json::from_str::<Value>(find("KARS_MISSION_ADMISSION")["value"].as_str().unwrap())
            .unwrap(),
        admission::for_task(&prepared.task).unwrap()
    );
    assert!(find("KARS_MISSION_ADMISSION").get("valueFrom").is_none());
    assert_eq!(
        find("KARS_MISSION_POD_UID")["valueFrom"]["fieldRef"]["fieldPath"],
        "metadata.uid"
    );
    assert_eq!(
        find("KARS_MISSION_IDENTITY_ROOT")["valueFrom"]["secretKeyRef"],
        json!({"name":roots::RUNTIME_SECRET,"key":"root","optional":false})
    );
    assert_eq!(serialized["spec"]["strategy"], json!({"type":"Recreate"}));
    assert_eq!(
        annotation(&prepared.sandbox.metadata, roots::PIN),
        Some(prepared.runtime_root.uid.as_str())
    );
    let secret: Value = f.get(ROOT);
    assert_eq!(secret["immutable"], true);
    assert_eq!(
        secret["metadata"]["ownerReferences"][0]["uid"],
        "runtime-uid"
    );
    assert_ne!(
        secret["data"]["root"],
        f.get::<Value>(HELPER_ROOT)["data"]["root"]
    );
    let root_again = roots::ensure(
        &f.client,
        roots::Anchor::Runtime(&f.get(SANDBOX)),
        &f.get(NS),
    )
    .await
    .unwrap();
    assert_eq!(root_again.value, prepared.runtime_root.value);
    assert_eq!(f.writes("/api/v1/namespaces/kars-run/secrets"), 1);
}

#[tokio::test]
async fn task_namespace_and_launch_fences_precede_all_secret_access() {
    for case in 0..6 {
        let f = Fixture::new().await;
        match case {
            0 => f.mutate(TASK, |v| v["metadata"]["uid"] = "replacement".into()),
            1 => f.mutate(NS, |v| {
                v["metadata"]["annotations"][namespace_ownership::SOURCE_UID] = "foreign".into()
            }),
            2 => f.mutate(TASK, |v| v["spec"]["execution"]["launch"] = false.into()),
            3 => f.mutate(SANDBOX, |v| v["spec"]["suspended"] = true.into()),
            4 => f.mutate(SANDBOX, |v| {
                v["metadata"]["annotations"]["kars.azure.com/credential-rebind-task-uid"] =
                    "".into()
            }),
            _ => f.mutate(TASK, |v| v["status"]["observedGeneration"] = 0.into()),
        }
        let result = prepare_with(
            &f.client,
            &f.get(SANDBOX),
            &mut f.get(DEPLOYMENT),
            &config(),
        )
        .await;
        if case < 2 {
            assert!(result.is_err());
        } else {
            assert!(result.unwrap().is_none());
        }
        assert!(
            f.state
                .lock()
                .unwrap()
                .calls
                .iter()
                .all(|(_, p, _, _)| !p.contains("/secrets"))
        );
    }
}

#[tokio::test]
async fn helper_root_bootstraps_before_ready_without_starting_the_runtime() {
    let f = Fixture::new().await;
    f.mutate(HELPER, |v| v["status"]["readyReplicas"] = 0.into());
    let mut desired = f.get(DEPLOYMENT);
    assert!(matches!(
        prepare_with(&f.client, &f.get(SANDBOX), &mut desired, &config()).await,
        Err(Error::NotReady(_))
    ));
    assert!(f.state.lock().unwrap().objects.contains_key(HELPER_ROOT));
    assert!(!f.state.lock().unwrap().objects.contains_key(ROOT));
    assert!(
        !serde_json::to_string(&desired)
            .unwrap()
            .contains("KARS_MISSION_")
    );
}

#[tokio::test]
async fn interrupted_root_creation_requires_exact_admin_recovery_and_never_rotates() {
    for ambiguous in [false, true] {
        let f = Fixture::new().await;
        f.state.lock().unwrap().fail = Some(if ambiguous {
            (
                "POST".into(),
                "/api/v1/namespaces/kars-run/secrets".into(),
                500,
                true,
            )
        } else {
            ("PATCH".into(), SANDBOX.into(), 409, false)
        });
        let ensure = || async {
            roots::ensure(
                &f.client,
                roots::Anchor::Runtime(&f.get(SANDBOX)),
                &f.get(NS),
            )
            .await
        };
        assert!(ensure().await.is_err());
        let secret: Value = f.get(ROOT);
        let uid = secret["metadata"]["uid"].as_str().unwrap();
        assert!(ensure().await.is_err());
        assert_eq!(f.body_reads(ROOT), 0);
        f.mutate(NS, |v| {
            v["metadata"]["annotations"][RECOVERY] = "wrong-uid".into()
        });
        assert!(ensure().await.is_err());
        assert_eq!(f.body_reads(ROOT), 0);
        f.mutate(NS, |v| v["metadata"]["annotations"][RECOVERY] = uid.into());
        let root = ensure().await.unwrap();
        assert_eq!(root.uid, uid);
        assert_eq!(f.get::<Value>(ROOT), secret);
        assert_eq!(f.writes("/api/v1/namespaces/kars-run/secrets"), 1);
        assert_eq!(
            f.get::<Value>(SANDBOX)["metadata"]["annotations"][roots::PIN],
            uid
        );
    }
}

#[tokio::test]
async fn rejects_replaced_or_foreign_root_metadata_before_body_read() {
    for case in 0..5 {
        let f = Fixture::new().await;
        let (prepared, _) = f.prepare().await;
        let reads = f.body_reads(ROOT);
        f.mutate(ROOT, |v| match case {
            0 => v["metadata"]["uid"] = "replacement".into(),
            1 => v["metadata"]["ownerReferences"][0]["uid"] = "foreign".into(),
            2 => {
                v["metadata"]["annotations"]["kars.azure.com/mission-owner-uid"] = "foreign".into()
            }
            3 => {
                v["metadata"]["annotations"]["kars.azure.com/mission-identity-role"] =
                    "dispatcher".into()
            }
            _ => v["metadata"]["deletionTimestamp"] = "2026-01-01T00:00:00Z".into(),
        });
        let error = roots::ensure(
            &f.client,
            roots::Anchor::Runtime(&prepared.sandbox),
            &prepared.namespace,
        )
        .await
        .err()
        .unwrap();
        assert!(!error.to_string().contains(&prepared.runtime_root.value));
        assert_eq!(f.body_reads(ROOT), reads);
    }
}

#[tokio::test]
async fn root_rechecks_are_metadata_only_and_detect_snapshot_changes() {
    let f = Fixture::new().await;
    let (prepared, _) = f.prepare().await;
    let reads = f.body_reads(ROOT);
    roots::recheck(
        &f.client,
        roots::Anchor::Runtime(&prepared.sandbox),
        &prepared.namespace,
        &prepared.runtime_root.uid,
    )
    .await
    .unwrap();
    assert_eq!(f.body_reads(ROOT), reads);
    f.mutate(SANDBOX, |v| v["metadata"]["resourceVersion"] = "99".into());
    assert!(
        roots::recheck(
            &f.client,
            roots::Anchor::Runtime(&prepared.sandbox),
            &prepared.namespace,
            &prepared.runtime_root.uid
        )
        .await
        .is_err()
    );
}

#[test]
fn workload_validation_rejects_stale_generations_owners_templates_and_readiness() {
    let (d, r, p) = workload_objects("kars-run", "run", "openclaw");
    let validate = |d: Value, r: Value, p: Value| {
        workload::validate(&workload::Workload {
            deployment: serde_json::from_value(d).unwrap(),
            replica_set: serde_json::from_value(r).unwrap(),
            pod: serde_json::from_value(p).unwrap(),
        })
    };
    validate(d.clone(), r.clone(), p.clone()).unwrap();
    for (which, path, value) in [
        (0, "/status/observedGeneration", json!(0)),
        (0, "/metadata/generation", json!(0)),
        (0, "/status/updatedReplicas", json!(0)),
        (0, "/status/replicas", json!(2)),
        (1, "/metadata/ownerReferences/0/uid", json!("foreign")),
        (
            1,
            "/metadata/annotations/deployment.kubernetes.io~1revision",
            json!("2"),
        ),
        (
            1,
            "/spec/template/spec/containers/0/image",
            json!("changed:latest"),
        ),
        (1, "/status/availableReplicas", json!(0)),
        (2, "/metadata/ownerReferences/0/uid", json!("foreign")),
        (2, "/metadata/labels/pod-template-hash", json!("old")),
        (2, "/status/conditions/0/status", json!("False")),
        (2, "/status/phase", json!("Pending")),
    ] {
        let mut values = [d.clone(), r.clone(), p.clone()];
        *values[which].pointer_mut(path).unwrap() = value;
        let [a, b, c] = values;
        assert!(validate(a, b, c).is_err(), "{path}");
    }
}

#[tokio::test]
async fn workload_reads_reject_overlap_truncation_and_changed_reverse_snapshot() {
    for case in 0..4 {
        let f = Fixture::new().await;
        let pods = "/api/v1/namespaces/kars-run/pods";
        match case {
            0 | 1 => {
                let mut pod: Value = f.get(&format!("{pods}/run-hash-pod"));
                pod["metadata"]["name"] = "old-pod".into();
                pod["metadata"]["uid"] = "old-uid".into();
                pod["metadata"].as_object_mut().unwrap().remove("labels");
                if case == 1 {
                    pod["metadata"]["deletionTimestamp"] = "2026-01-01T00:00:00Z".into();
                }
                f.set(&format!("{pods}/old-pod"), &pod);
            }
            2 => f.state.lock().unwrap().truncate = Some(pods.into()),
            _ => f.state.lock().unwrap().change_on_get = Some((format!("{pods}/run-hash-pod"), 1)),
        }
        assert!(workload::read(&f.client, f.get(DEPLOYMENT)).await.is_err());
    }
}

#[tokio::test]
async fn admission_projection_is_stable_on_ack_but_changes_for_new_run_authority() {
    let f = Fixture::new().await;
    let original: Deployment = f.get(DEPLOYMENT);
    let (prepared, expected) = f.prepare().await;
    let project_task = |task: &KarsTask| {
        let mut desired = original.clone();
        project(
            &mut desired,
            task,
            &prepared.sandbox,
            &prepared.runtime_root.uid,
            &prepared.dispatcher_did,
        )
        .unwrap();
        desired
    };
    let mut acknowledged = prepared.task.clone();
    for key in ["kars.azure.com/run-ack", "kars.azure.com/run-completed"] {
        acknowledged
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(key.into(), "preserved-run".into());
    }
    assert_eq!(project_task(&acknowledged), expected);
    for change in ["run", "objective", "generation", "authority"] {
        let mut task = acknowledged.clone();
        match change {
            "run" => {
                task.metadata
                    .annotations
                    .as_mut()
                    .unwrap()
                    .insert("kars.azure.com/run-requested".into(), "next-run".into());
            }
            "objective" => task.spec.objective.push('!'),
            "generation" => task.metadata.generation = Some(2),
            "authority" => task.spec.envelope.delegation_depth += 1,
            _ => unreachable!(),
        }
        assert_ne!(
            project_task(&task).spec.unwrap().template,
            expected.spec.as_ref().unwrap().template,
            "{change}"
        );
    }
}

#[tokio::test]
async fn binding_rejects_a_ready_workload_with_a_different_installed_admission() {
    let f = Fixture::new().await;
    let prepared = f.applied().await;
    let mut deployment: Value = f.get(DEPLOYMENT);
    let env = deployment["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_array_mut()
        .unwrap();
    let installed = env
        .iter_mut()
        .find(|e| e["name"] == "KARS_MISSION_ADMISSION")
        .unwrap();
    let mut admission = admission::for_task(&prepared.task).unwrap();
    admission["runNonce"] = "other-run".into();
    installed["value"] = admission.to_string().into();
    f.install_workload(deployment);
    assert!(publish(&f.client, &prepared).await.is_err());
    assert_eq!(f.writes("/api/v1/namespaces/work/configmaps"), 0);
    assert_eq!(f.writes(TASK), 0);
}

#[tokio::test]
async fn publishes_actual_uid_binding_and_repeated_publication_is_a_read_only_noop() {
    let f = Fixture::new().await;
    let original: Value = f.get(TASK);
    let prepared = f.applied().await;
    publish(&f.client, &prepared).await.unwrap();
    let map: Value = f.get(BINDING);
    let binding: Value =
        serde_json::from_str(map["data"]["binding.json"].as_str().unwrap()).unwrap();
    assert_eq!(
        binding,
        json!({"taskName":"run","taskUid":"task-uid","sandboxName":"run","sandboxUid":"sandbox-uid",
        "namespaceUid":"runtime-uid","deploymentUid":"run-deployment","deploymentGeneration":1,"replicaSetName":"run-hash",
        "replicaSetUid":"run-rs","podName":"run-hash-pod","podUid":"run-pod","dispatcherDid":prepared.dispatcher_did,
        "agentDid":identity::did(&prepared.runtime_root.value,identity::Role::Runtime,"sandbox-uid","run-pod").unwrap(),
        "admission":admission::for_task(&prepared.task).unwrap()})
    );
    assert_eq!(map["metadata"]["ownerReferences"][0]["uid"], "task-uid");
    publish(&f.client, &prepared).await.unwrap();
    assert_eq!(f.get::<Value>(BINDING), map);
    assert_eq!(f.writes("/api/v1/namespaces/work/configmaps"), 1);
    assert_eq!(f.get::<Value>(TASK), original);
    assert_eq!(f.writes(TASK), 0);
}

#[tokio::test]
async fn binding_update_is_uid_rv_cas_and_conflicts_are_never_adopted() {
    let f = Fixture::new().await;
    let prepared = f.applied().await;
    publish(&f.client, &prepared).await.unwrap();
    f.mutate(BINDING, |v| v["data"]["binding.json"] = "{}".into());
    f.state.lock().unwrap().fail = Some(("PUT".into(), BINDING.into(), 409, false));
    let error = publish(&f.client, &prepared).await.unwrap_err().to_string();
    assert!(!error.contains("sensitive upstream"));
    assert_eq!(f.get::<Value>(BINDING)["data"]["binding.json"], "{}");
    assert_eq!(f.writes(BINDING), 1);
    publish(&f.client, &prepared).await.unwrap();
    let s = f.state.lock().unwrap();
    let (_, _, _, body) = s
        .calls
        .iter()
        .rev()
        .find(|(m, p, _, _)| m == "PUT" && p == BINDING)
        .unwrap();
    assert_eq!(body["metadata"]["uid"], "created-kars-mission-binding-run");
    assert_eq!(body["metadata"]["resourceVersion"], "1");
}

#[tokio::test]
async fn binding_rejects_foreign_existing_map_and_raced_workload_or_task() {
    for case in 0..4 {
        let f = Fixture::new().await;
        let prepared = f.applied().await;
        match case {
            0=>f.set(BINDING,&json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{
                "name":"kars-mission-binding-run","namespace":"work","uid":"foreign","resourceVersion":"1"}})),
            1=>f.state.lock().unwrap().change_on_get=Some((TASK.into(),1)),
            2=>f.mutate(DEPLOYMENT,|v|v["spec"]["template"]["spec"]["containers"][0]["env"][0]["value"]="false".into()),
            _=>f.mutate(DEPLOYMENT,|v|{v["metadata"]["annotations"].as_object_mut().unwrap().remove("kars.azure.com/credential-sandbox-uid");}),
        }
        assert!(publish(&f.client, &prepared).await.is_err());
        assert_eq!(f.writes("/api/v1/namespaces/work/configmaps"), 0);
    }
}

#[tokio::test]
async fn transient_helper_convergence_preserves_existing_runtime_but_defers_a_new_one() {
    let f = Fixture::new().await;
    let prepared = f.applied().await;
    let prior: Deployment = f.get(DEPLOYMENT);
    let mut desired: Deployment =
        serde_json::from_value(workload_objects("kars-run", "run", "openclaw").0).unwrap();
    defer_until_ready(&f.client, &prepared.sandbox, &mut desired)
        .await
        .unwrap();
    assert_eq!(desired.spec, prior.spec);
    assert_eq!(desired.metadata.uid, prior.metadata.uid);
    assert_eq!(
        desired.metadata.resource_version,
        prior.metadata.resource_version
    );
    assert_eq!(f.writes(DEPLOYMENT), 0);
    f.state.lock().unwrap().objects.remove(DEPLOYMENT);
    assert!(matches!(
        defer_until_ready(&f.client, &prepared.sandbox, &mut desired).await,
        Err(Error::NotReady(_))
    ));
    assert_eq!(f.writes(DEPLOYMENT), 0);
    desired.spec = None;
    assert!(matches!(
        defer_until_ready(&f.client, &prepared.sandbox, &mut desired).await,
        Err(Error::Invalid(_))
    ));
}

#[test]
fn snapshots_distinguish_convergence_from_identity_loss() {
    let prior = ObjectMeta {
        name: Some("runtime".into()),
        namespace: Some("work".into()),
        uid: Some("uid".into()),
        resource_version: Some("1".into()),
        ..Default::default()
    };
    assert!(same_snapshot(&prior, &prior).is_ok());
    let mut current = prior.clone();
    current.resource_version = Some("2".into());
    assert!(matches!(
        same_snapshot(&prior, &current),
        Err(Error::NotReady(_))
    ));
    for field in ["uid", "name", "namespace", "deleting", "missing-version"] {
        let mut current = prior.clone();
        match field {
            "uid" => current.uid = Some("replacement".into()),
            "name" => current.name = Some("replacement".into()),
            "namespace" => current.namespace = Some("replacement".into()),
            "deleting" => {
                current.deletion_timestamp =
                    Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                        k8s_openapi::jiff::Timestamp::now(),
                    ))
            }
            "missing-version" => current.resource_version = None,
            _ => unreachable!(),
        }
        assert!(
            matches!(same_snapshot(&prior, &current), Err(Error::Invalid(_))),
            "{field}"
        );
    }
}

#[tokio::test]
async fn post_pin_snapshot_change_never_projects_identity() {
    let f = Fixture::new().await;
    // First GET + root bootstrap's three metadata GETs precede the post-pin read.
    f.state.lock().unwrap().change_on_get = Some((SANDBOX.into(), 5));
    let mut desired = f.get(DEPLOYMENT);
    let result = prepare_with(&f.client, &f.get(SANDBOX), &mut desired, &config()).await;
    assert!(result.is_err());
    assert!(
        !serde_json::to_string(&desired)
            .unwrap()
            .contains("KARS_MISSION_")
    );
}
