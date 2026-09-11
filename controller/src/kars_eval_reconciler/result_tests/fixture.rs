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
    pub lose_ack: bool,
    pub log_error: bool,
    pub mutate_on_log: Option<&'static str>,
}

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({"apiVersion":"v1","kind":"Status",
        "status":"Failure","code":code,"reason":if code == 404 {"NotFound"} else {"Error"},
        "message":"private server diagnostic"}))
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
                "report-uid"
            } else if base == CRONS {
                "cron-uid"
            } else {
                "job-uid"
            });
            value["metadata"]["resourceVersion"] = json!("20");
            value["metadata"]["generation"] = json!(1);
            value["metadata"]["creationTimestamp"] = json!("2026-09-10T20:00:00Z");
        } else if method == "PUT" && is_report {
            let old = store.cms.get(&name).unwrap();
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
            store.report_writes += 1;
            if store.lose_ack {
                store.lose_ack = false;
                return failure(500);
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

pub async fn setup() -> Fixture {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut eval = KarsEval::new("eval", Default::default());
    eval.spec.target_sandbox_ref.name = "demo".into();
    eval.metadata.namespace = Some("tenant".into());
    eval.metadata.uid = Some("eval-uid".into());
    eval.metadata.generation = Some(1);
    eval.metadata.resource_version = Some("10".into());
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
        lose_ack: false,
        log_error: false,
        mutate_on_log: None,
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
