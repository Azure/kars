// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::super::*;
use k8s_openapi::api::core::v1::Pod;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const PREFIX: &str = "/apis/kars.azure.com/v1alpha1/namespaces/tenant";
const JOBS: &str = "/apis/batch/v1/namespaces/tenant/jobs";
const CRONS: &str = "/apis/batch/v1/namespaces/tenant/cronjobs";
const PODS: &str = "/api/v1/namespaces/tenant/pods";
const CMS: &str = "/api/v1/namespaces/tenant/configmaps";

pub struct Store {
    pub eval: Value,
    pub target: Value,
    pub jobs: BTreeMap<String, Value>,
    pub crons: BTreeMap<String, Value>,
    pub pods: BTreeMap<String, Value>,
    pub cms: BTreeMap<String, Value>,
    pub report: Value,
    pub report_writes: usize,
    pub eval_status_writes: usize,
    pub target_status_writes: usize,
    pub job_serial: usize,
    pub lose_ack: bool,
    pub log_error: bool,
    pub mutate_on_log: Option<&'static str>,
    pub mutate_on_target_patch: Option<&'static str>,
}

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({"apiVersion":"v1","kind":"Status",
        "status":"Failure","code":code,"reason":if code == 404 {"NotFound"} else {"Error"},
        "message":"private server diagnostic"}))
}

fn merge(current: &mut Value, patch: &Value) {
    if let Some(fields) = patch.as_object() {
        if !current.is_object() {
            *current = json!({});
        }
        for (key, value) in fields {
            if value.is_null() {
                current.as_object_mut().unwrap().remove(key);
            } else {
                merge(&mut current[key], value);
            }
        }
    } else {
        *current = patch.clone();
    }
}

fn status_patch(current: &mut Value, request: &Request) -> ResponseTemplate {
    let patch: Value = serde_json::from_slice(&request.body).unwrap();
    if patch["metadata"]["uid"] != current["metadata"]["uid"]
        || patch["metadata"]["resourceVersion"] != current["metadata"]["resourceVersion"]
    {
        return failure(409);
    }
    merge(&mut current["status"], &patch["status"]);
    let rv = current["metadata"]["resourceVersion"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        + 1;
    current["metadata"]["resourceVersion"] = json!(rv.to_string());
    ResponseTemplate::new(200).set_body_json(current.clone())
}

#[derive(Clone)]
struct Server(Arc<Mutex<Store>>);
impl Respond for Server {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut store = self.0.lock().unwrap();
        let path = request.url.path();
        let method = request.method.as_str();
        if method == "GET" && path == format!("{PREFIX}/karsevals/eval") {
            return ResponseTemplate::new(200).set_body_json(&store.eval);
        }
        if method == "PATCH" && path == format!("{PREFIX}/karsevals/eval/status") {
            store.eval_status_writes += 1;
            return status_patch(&mut store.eval, request);
        }
        if method == "PATCH" && path == format!("{PREFIX}/karssandboxes/demo/status") {
            match store.mutate_on_target_patch.take() {
                Some("uid") => store.target["metadata"]["uid"] = json!("replacement"),
                Some("rv") => store.target["metadata"]["resourceVersion"] = json!("99"),
                None => {}
                Some(_) => panic!("unknown target patch mutation"),
            }
            store.target_status_writes += 1;
            return status_patch(&mut store.target, request);
        }
        if method == "PATCH" && path == format!("{PREFIX}/karsevals/eval") {
            let patch: Value = serde_json::from_slice(&request.body).unwrap();
            if patch["metadata"]["uid"] != store.eval["metadata"]["uid"]
                || patch["metadata"]["resourceVersion"] != store.eval["metadata"]["resourceVersion"]
            {
                return failure(409);
            }
            if !store.eval["metadata"]["annotations"].is_object() {
                store.eval["metadata"]["annotations"] = json!({});
            }
            for (key, value) in patch["metadata"]["annotations"].as_object().unwrap() {
                let annotations = store.eval["metadata"]["annotations"]
                    .as_object_mut()
                    .unwrap();
                if value.is_null() {
                    annotations.remove(key);
                } else {
                    annotations.insert(key.clone(), value.clone());
                }
            }
            let rv = store.eval["metadata"]["resourceVersion"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                + 1;
            store.eval["metadata"]["resourceVersion"] = json!(rv.to_string());
            return ResponseTemplate::new(200).set_body_json(&store.eval);
        }
        if method == "GET" && path == format!("{PREFIX}/karssandboxes/demo") {
            return ResponseTemplate::new(200).set_body_json(&store.target);
        }
        if method == "GET" && path.ends_with("/log") {
            if store.log_error {
                return failure(500);
            }
            let mutation = store.mutate_on_log.take();
            match mutation {
                Some("eval-uid") => store.eval["metadata"]["uid"] = json!("replacement"),
                Some("eval-gen") => store.eval["metadata"]["generation"] = json!(2),
                Some("eval-spec") => store.eval["spec"]["displayName"] = json!("changed"),
                Some("target-uid") => store.target["metadata"]["uid"] = json!("replacement"),
                Some("target-gen") => store.target["metadata"]["generation"] = json!(2),
                Some("target-spec") => {
                    store.target["spec"]["inferenceRef"]["name"] = json!("changed")
                }
                Some("job-uid") => {
                    store.jobs.values_mut().next().unwrap()["metadata"]["uid"] =
                        json!("replacement")
                }
                Some("job-gen") => {
                    store.jobs.values_mut().next().unwrap()["metadata"]["generation"] = json!(2)
                }
                Some("job-token") => {
                    store.jobs.values_mut().next().unwrap()["metadata"]["annotations"]
                        [workloads::RUN_TOKEN] = json!("wrong-token")
                }
                Some("pod-uid") => {
                    store.pods.values_mut().next().unwrap()["metadata"]["uid"] =
                        json!("replacement")
                }
                Some("pod-gen") => {
                    store.pods.values_mut().next().unwrap()["metadata"]["generation"] = json!(2)
                }
                Some("pod-spec") => {
                    store.pods.values_mut().next().unwrap()["spec"]["containers"][0]["image"] =
                        json!("foreign")
                }
                None => {}
                Some(_) => panic!("unknown fixture mutation"),
            }
            return ResponseTemplate::new(200).set_body_string(store.report.to_string());
        }
        if method == "GET" && matches!(path, JOBS | PODS) {
            let (kind, items) = if path == JOBS {
                ("JobList", &store.jobs)
            } else {
                ("PodList", &store.pods)
            };
            return ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion":if path == JOBS {"batch/v1"} else {"v1"},"kind":kind,
                "metadata":{},"items":items.values().collect::<Vec<_>>(),
            }));
        }
        let (base, is_report) = if path.starts_with(JOBS) {
            (JOBS, false)
        } else if path.starts_with(CRONS) {
            (CRONS, false)
        } else if path.starts_with(PODS) {
            (PODS, false)
        } else if path.starts_with(CMS) {
            (CMS, true)
        } else {
            return failure(404);
        };
        let name = path.strip_prefix(base).unwrap().trim_start_matches('/');
        if method == "GET" {
            let value = if is_report {
                store.cms.get(name)
            } else if base == CRONS {
                store.crons.get(name)
            } else if base == PODS {
                store.pods.get(name)
            } else {
                store.jobs.get(name)
            };
            return value.map_or_else(
                || failure(404),
                |value| ResponseTemplate::new(200).set_body_json(value),
            );
        }
        let mut value: Value = serde_json::from_slice(&request.body).unwrap();
        let name = value["metadata"]["name"].as_str().unwrap().to_owned();
        if method == "POST" {
            if if is_report {
                store.cms.contains_key(&name)
            } else if base == CRONS {
                store.crons.contains_key(&name)
            } else {
                store.jobs.contains_key(&name)
            } {
                return failure(409);
            }
            value["metadata"]["uid"] = json!(if is_report {
                format!("cm-uid-{name}")
            } else if base == CRONS {
                "cron-uid".into()
            } else {
                store.job_serial += 1;
                format!("job-uid-{}", store.job_serial)
            });
            value["metadata"]["resourceVersion"] = json!("20");
            value["metadata"]["generation"] = json!(1);
            value["metadata"]["creationTimestamp"] = json!("2026-09-10T20:00:00Z");
        } else if method == "PUT" && (is_report || base == CRONS) {
            let old = if is_report {
                store.cms.get(&name)
            } else {
                store.crons.get(&name)
            }
            .unwrap();
            if old["metadata"]["uid"] != value["metadata"]["uid"]
                || old["metadata"]["resourceVersion"] != value["metadata"]["resourceVersion"]
            {
                return failure(409);
            }
            value["metadata"]["resourceVersion"] = json!("21");
        } else {
            return failure(405);
        }
        if is_report {
            store.cms.insert(name, value.clone());
            if value["metadata"]["name"]
                .as_str()
                .unwrap()
                .ends_with("-report")
            {
                store.report_writes += 1;
                if store.lose_ack {
                    store.lose_ack = false;
                    return failure(500);
                }
            }
        } else if base == CRONS {
            store.crons.insert(name, value.clone());
        } else {
            store.jobs.insert(name, value.clone());
        }
        ResponseTemplate::new(if method == "POST" { 201 } else { 200 }).set_body_json(value)
    }
}

pub struct Fixture {
    pub _server: MockServer,
    pub client: Client,
    pub eval: KarsEval,
    pub intent: workloads::Intent,
    pub corpus: ResolvedCorpus,
    pub store: Arc<Mutex<Store>>,
}

impl Fixture {
    pub async fn refresh_intent(&mut self) {
        self.store.lock().unwrap().eval = json!(self.eval);
        self.intent = workloads::Intent::new(
            &self.client,
            &self.eval,
            &self.corpus.digest,
            "custom-numeric-user:latest",
            &sandbox_router_url("demo"),
        )
        .await
        .unwrap();
    }

    pub async fn request(&mut self) -> String {
        let api = Api::<KarsEval>::namespaced(self.client.clone(), "tenant");
        self.eval
            .annotations_mut()
            .insert(ANNOTATION_RUN_NOW.into(), "true".into());
        self.store.lock().unwrap().eval = json!(self.eval);
        assert!(workloads::claim_trigger(&api, &self.eval).await.unwrap());
        self.eval = api.get("eval").await.unwrap();
        self.refresh_intent().await;
        let name = self.intent.request_job.clone().unwrap();
        ensure_run_now_job(
            &Api::<Job>::namespaced(self.client.clone(), "tenant"),
            &name,
            &self.eval,
            &self.intent,
            "karseval-eval-corpus",
            "custom-numeric-user:latest",
            &self.intent.router,
            &self.corpus.label,
        )
        .await
        .unwrap();
        let claimed_annotations = self.intent.annotations();
        self.eval = workloads::acknowledge_trigger(&api, &self.eval, &name)
            .await
            .unwrap();
        self.refresh_intent().await;
        assert_eq!(claimed_annotations, self.intent.annotations());
        name
    }

    pub fn complete(&self, name: &str) {
        let mut store = self.store.lock().unwrap();
        let job = store.jobs.get_mut(name).unwrap();
        job["status"] = json!({"succeeded":1,"conditions":[
            {"type":"Complete","status":"True","lastTransitionTime":"2026-09-10T20:00:05Z"}]});
        let mut pod = job["spec"]["template"].clone();
        pod["apiVersion"] = json!("v1");
        pod["kind"] = json!("Pod");
        pod["metadata"]["name"] = json!(format!("{name}-pod"));
        pod["metadata"]["namespace"] = json!("tenant");
        pod["metadata"]["uid"] = json!(format!("{name}-pod-uid"));
        pod["metadata"]["generation"] = json!(1);
        pod["metadata"]["resourceVersion"] = json!("30");
        pod["metadata"]["ownerReferences"] = json!([{"apiVersion":"batch/v1","kind":"Job",
            "name":name,"uid":job["metadata"]["uid"],"controller":true}]);
        pod["status"] = json!({"phase":"Succeeded","containerStatuses":[{"name":"runner",
            "image":"custom-numeric-user:latest","imageID":"image-id","ready":false,
            "restartCount":0,"state":{"terminated":{"exitCode":0,
                "startedAt":"2026-09-10T20:00:00Z","finishedAt":"2026-09-10T20:00:04Z"}}}]});
        store.pods.insert(format!("{name}-pod"), pod);
    }

    pub async fn reconcile(&mut self) -> KarsEvalStatus {
        self.store.lock().unwrap().eval = json!(self.eval);
        super::super::reconcile(
            Arc::new(self.eval.clone()),
            Arc::new(Ctx {
                client: self.client.clone(),
            }),
        )
        .await
        .unwrap();
        self.eval = serde_json::from_value(self.store.lock().unwrap().eval.clone()).unwrap();
        self.eval.status.clone().unwrap()
    }
}

pub async fn setup() -> Fixture {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut eval = KarsEval::new("eval", Default::default());
    eval.spec.target_sandbox_ref.name = "demo".into();
    eval.metadata.namespace = Some("tenant".into());
    eval.metadata.uid = Some("eval-uid".into());
    eval.metadata.generation = Some(1);
    eval.metadata.resource_version = Some("10".into());
    eval.metadata.finalizers = Some(vec![FINALIZER.into()]);
    eval.spec.runner_image = Some("custom-numeric-user:latest".into());
    let mut target = KarsSandbox::new("demo", Default::default());
    target.metadata.namespace = eval.namespace();
    target.metadata.uid = Some("target-uid".into());
    target.metadata.resource_version = Some("11".into());
    target.metadata.generation = Some(1);
    target.spec.inference_ref.name = "policy".into();
    target.spec.sandbox = Some(Default::default());
    let corpus = resolve_corpus(&eval.spec.corpus).await.unwrap();
    let (_, mut report) = report::tests::fixture();
    report["corpusDigest"] = json!(corpus.digest);
    let server = MockServer::start().await;
    let store = Arc::new(Mutex::new(Store {
        eval: json!(eval),
        target: json!(target),
        jobs: BTreeMap::new(),
        crons: BTreeMap::new(),
        pods: BTreeMap::new(),
        cms: BTreeMap::new(),
        report,
        report_writes: 0,
        eval_status_writes: 0,
        target_status_writes: 0,
        job_serial: 0,
        lose_ack: false,
        log_error: false,
        mutate_on_log: None,
        mutate_on_target_patch: None,
    }));
    Mock::given(wiremock::matchers::any())
        .respond_with(Server(store.clone()))
        .mount(&server)
        .await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    let image = "custom-numeric-user:latest";
    let router = sandbox_router_url("demo");
    let intent = workloads::Intent::new(&client, &eval, &corpus.digest, image, &router)
        .await
        .unwrap();
    let job_name = run_now_job_name("eval", Some("claimed-token"));
    ensure_run_now_job(
        &Api::<Job>::namespaced(client.clone(), "tenant"),
        &job_name,
        &eval,
        &intent,
        "karseval-eval-corpus",
        image,
        &router,
        &corpus.label,
    )
    .await
    .unwrap();
    {
        let mut state = store.lock().unwrap();
        let job = state.jobs.get_mut(&job_name).unwrap();
        job["status"] = json!({"succeeded":1,"conditions":[{"type":"Complete","status":"True","lastTransitionTime":"2026-09-10T20:00:05Z"}]});
        let mut pod = job["spec"]["template"].clone();
        pod["apiVersion"] = json!("v1");
        pod["kind"] = json!("Pod");
        pod["metadata"]["name"] = json!("runner-pod");
        pod["metadata"]["namespace"] = json!("tenant");
        pod["metadata"]["uid"] = json!("pod-uid");
        pod["metadata"]["generation"] = json!(1);
        pod["metadata"]["resourceVersion"] = json!("30");
        pod["metadata"]["ownerReferences"] = json!([{"apiVersion":"batch/v1","kind":"Job",
            "name":job_name,"uid":job["metadata"]["uid"],"controller":true}]);
        pod["status"] = json!({"phase":"Succeeded","containerStatuses":[{"name":"runner",
        "image":image,"imageID":"image-id","ready":false,"restartCount":0,"state":{"terminated":{
            "exitCode":0,"startedAt":"2026-09-10T20:00:00Z","finishedAt":"2026-09-10T20:00:04Z",
        }}}]});
        let _: Pod = serde_json::from_value(pod.clone()).unwrap();
        state.pods.insert("runner-pod".into(), pod);
    }
    Fixture {
        _server: server,
        client,
        eval,
        intent,
        corpus,
        store,
    }
}
