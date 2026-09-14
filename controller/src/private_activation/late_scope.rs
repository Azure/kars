// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! An operator-owned late-enrollment receipt is also a controller resume fence.
//! A concurrent Sandbox unsuspend must not bypass post-retirement key checks.

use super::{EPOCH, hash};
use crate::crd::KarsSandbox;
use base64::{Engine, engine::general_purpose::STANDARD};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Secret},
};
use kube::{
    Api, Client, ResourceExt,
    core::{ApiResource, DynamicObject, GroupVersionKind},
};
use serde_json::{Value, json};

const HISTORY: &str = "kars.azure.com/private-root-retirement";
const ERROR: &str = "Late private runtime source, owner, intent or authentication changed; operator recovery remains required";
const ADMIN: &str = "router-services-admin";
const VERSION: &str = "kars.azure.com/services-credential-version";

fn fields(value: &Value, required: &str, optional: &str) -> bool {
    value.as_object().is_some_and(|object| {
        required
            .split_whitespace()
            .all(|key| object.contains_key(key))
            && object.keys().all(|key| {
                required
                    .split_whitespace()
                    .chain(optional.split_whitespace())
                    .any(|allowed| allowed == key.as_str())
            })
    })
}
fn text(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|value| !value.is_empty() && value.len() <= 253)
}
fn hex(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}
fn identity(value: &Value) -> bool {
    fields(value, "name uid resourceVersion", "")
        && ["name", "uid", "resourceVersion"]
            .iter()
            .all(|key| text(&value[key]))
}
fn material(value: &Value) -> bool {
    fields(value, "object key", "")
        && identity(&value["object"])
        && value["object"]["name"] == ADMIN
        && hex(&value["key"])
}

fn decode(raw: &str) -> Result<Value, String> {
    if raw.len() > 131_072 {
        return Err(ERROR.into());
    }
    let state: Value = serde_json::from_str(raw).map_err(|_| ERROR)?;
    let phase = state["phase"].as_str().ok_or(ERROR)?;
    let epoch = state.get("epoch");
    // v1 retirement and v2 completion belong to the shared root, not runtime
    // namespaces. v3 is the only preceding scoped receipt protocol.
    match state["version"].as_u64() {
        Some(3)
            if fields(&state, "version root binding phase", "epoch")
                && hex(&state["root"])
                && hex(&state["binding"])
                && match phase {
                    "Pending" => epoch.is_none(),
                    "Stamping" | "Qualified" => epoch.is_some_and(hex),
                    _ => false,
                } =>
        {
            return Ok(state);
        }
        Some(4) => {}
        _ => return Err(ERROR.into()),
    }
    let runtime = &state["runtime"];
    let qualified = state.get("qualified");
    let task_valid = runtime.get("task").is_none_or(|task| {
        fields(task, "object spec generation authorization", "")
            && identity(&task["object"])
            && hex(&task["spec"])
            && task["generation"]
                .as_i64()
                .is_some_and(|generation| generation > 0)
            && task_authorization(&task["authorization"]).is_ok()
    });
    let phase_valid = match phase {
        "Pausing" | "Retired" => epoch.is_none() && qualified.is_none(),
        "Rotating" => epoch.is_some_and(hex) && qualified.is_none(),
        "Restoring" | "Qualified" => {
            epoch.is_some_and(hex)
                && qualified.is_some_and(|value| {
                    fields(value, "binding template material", "")
                        && hex(&value["binding"])
                        && hex(&value["template"])
                        && material(&value["material"])
                        && value["material"]["object"]["uid"] == state["baseline"]["object"]["uid"]
                        && value["material"]["object"]["resourceVersion"]
                            != state["baseline"]["object"]["resourceVersion"]
                        && value["material"]["key"] != state["baseline"]["key"]
                })
        }
        _ => false,
    };
    if !fields(
        &state,
        "version root binding attempt phase runtime deployment structure replicas captured baseline",
        "epoch qualified",
    ) || !["root", "binding", "attempt", "structure"]
        .iter()
        .all(|key| hex(&state[key]))
        || !fields(
            runtime,
            "sandbox workspace spec owners generation suspended",
            "task",
        )
        || !identity(&runtime["sandbox"])
        || !text(&runtime["workspace"])
        || !hex(&runtime["spec"])
        || !hex(&runtime["owners"])
        || !runtime["generation"]
            .as_i64()
            .is_some_and(|generation| generation > 0)
        || !(runtime["suspended"].is_null() || runtime["suspended"].is_boolean())
        || !identity(&state["deployment"])
        || !material(&state["baseline"])
        || state["replicas"].as_u64() != Some(u64::from(runtime["suspended"] != true))
        || !state["captured"]
            .as_array()
            .is_some_and(|ids| ids.iter().all(text))
        || !task_valid
        || !phase_valid
    {
        return Err(ERROR.into());
    }
    Ok(state)
}

async fn object(
    client: &Client,
    workspace: &str,
    kind: &str,
    plural: &str,
    name: &str,
) -> Result<Value, String> {
    let mut resource =
        ApiResource::from_gvk(&GroupVersionKind::gvk("kars.azure.com", "v1alpha1", kind));
    resource.plural = plural.into();
    let value = Api::<DynamicObject>::namespaced_with(client.clone(), workspace, &resource)
        .get(name)
        .await
        .map_err(|_| ERROR)?;
    serde_json::to_value(value).map_err(|_| ERROR.into())
}

fn live(value: &Value, expected: &Value) -> bool {
    value["metadata"]["uid"]
        .as_str()
        .is_some_and(|uid| !uid.is_empty())
        && value["metadata"]["uid"] == expected["uid"]
        && value["metadata"]["name"] == expected["name"]
        && value["metadata"]["resourceVersion"]
            .as_str()
            .is_some_and(|rv| !rv.is_empty())
        && value["metadata"]["deletionTimestamp"].is_null()
}

fn owners(value: &Value) -> Value {
    value["metadata"]
        .get("ownerReferences")
        .cloned()
        .unwrap_or_else(|| json!([]))
}

fn task_authorization(value: &Value) -> Result<&str, String> {
    let value = value.as_str().ok_or(ERROR)?;
    let digest = value.strip_prefix("sha256:").ok_or(ERROR)?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ERROR.into());
    }
    Ok(value)
}

fn intent(state: &Value, sandbox: &Value, task: Option<&Value>) -> Result<bool, String> {
    let runtime = &state["runtime"];
    let phase = state["phase"].as_str().ok_or(ERROR)?;
    if !["Pausing", "Retired", "Rotating", "Restoring"].contains(&phase)
        || !live(sandbox, &runtime["sandbox"])
        || sandbox["metadata"]["namespace"] != runtime["workspace"]
        || hash(&owners(sandbox)) != runtime["owners"].as_str().ok_or(ERROR)?
    {
        return Err(ERROR.into());
    }
    let mut spec = sandbox["spec"].as_object().ok_or(ERROR)?.clone();
    let suspension = spec.remove("suspended").unwrap_or(Value::Null);
    let original = &runtime["suspended"];
    if (!suspension.is_null() && !suspension.is_boolean())
        || (!original.is_null() && !original.is_boolean())
        || hash(&Value::Object(spec)) != runtime["spec"].as_str().ok_or(ERROR)?
        || (suspension != *original && suspension != true)
    {
        return Err(ERROR.into());
    }
    let generation = runtime["generation"]
        .as_i64()
        .filter(|v| *v > 0)
        .ok_or(ERROR)?;
    let increment = if *original == true {
        0
    } else if suspension == true {
        1
    } else if phase == "Restoring" {
        2
    } else {
        0
    };
    if sandbox["metadata"]["generation"].as_i64() != generation.checked_add(increment) {
        return Err(ERROR.into());
    }
    match (runtime.get("task"), task) {
        (Some(expected), Some(task)) => {
            if !live(task, &expected["object"])
                || task["metadata"]["namespace"] != runtime["workspace"]
                || task["metadata"]["generation"] != expected["generation"]
                || task["status"]["observedGeneration"] != expected["generation"]
                || task["status"]["phase"] != "Ready"
                || task_authorization(&task["status"]["envelopeDigest"])?
                    != task_authorization(&expected["authorization"])?
                || task["spec"]["execution"]["launch"] != true
                || hash(&json!({"spec":task["spec"],"owners":owners(task)}))
                    != expected["spec"].as_str().ok_or(ERROR)?
            {
                return Err(ERROR.into());
            }
        }
        (None, None) => {}
        _ => return Err(ERROR.into()),
    }
    Ok(phase != "Restoring" || suspension == true)
}

fn rotated(
    state: &Value,
    secret: &Secret,
    namespace: &Namespace,
    deployment: &Deployment,
) -> Result<(), String> {
    let material = &state["qualified"]["material"];
    let baseline = &state["baseline"];
    let epoch = state["epoch"]
        .as_str()
        .filter(|v| v.len() == 64)
        .ok_or(ERROR)?;
    let token = secret
        .data
        .as_ref()
        .and_then(|data| data.get("control-token"))
        .ok_or(ERROR)?;
    let digest = hash(&json!(STANDARD.encode(&token.0)));
    let version = format!(
        "{}:{}",
        secret.uid().ok_or(ERROR)?,
        secret.resource_version().ok_or(ERROR)?
    );
    let annotations = deployment
        .spec
        .as_ref()
        .and_then(|s| s.template.metadata.as_ref())
        .and_then(|m| m.annotations.as_ref())
        .ok_or(ERROR)?;
    if secret.metadata.deletion_timestamp.is_some()
        || secret.metadata.uid.as_deref() != material["object"]["uid"].as_str()
        || secret.metadata.uid.as_deref() != baseline["object"]["uid"].as_str()
        || secret.metadata.resource_version.as_deref()
            != material["object"]["resourceVersion"].as_str()
        || material["object"]["resourceVersion"] == baseline["object"]["resourceVersion"]
        || secret.type_.as_deref() != Some("Opaque")
        || secret.data.as_ref().is_none_or(|data| data.len() != 1)
        || token.0.len() != 64
        || !token.0.iter().all(u8::is_ascii_alphanumeric)
        || Some(digest.as_str()) != material["key"].as_str()
        || Some(digest.as_str()) == baseline["key"].as_str()
        || secret.annotations().get(EPOCH).map(String::as_str) != Some(epoch)
        || namespace.annotations().get(EPOCH).map(String::as_str) != Some(epoch)
        || annotations.get(EPOCH).map(String::as_str) != Some(epoch)
        || annotations.get(VERSION) != Some(&version)
    {
        return Err(ERROR.into());
    }
    Ok(())
}

pub(super) async fn fence(
    client: &Client,
    namespace: &Namespace,
    sandbox: &KarsSandbox,
    previous: Option<&Deployment>,
    deployment: &mut Deployment,
) -> Result<(), String> {
    let Some(raw) = namespace.annotations().get(HISTORY) else {
        return Ok(());
    };
    let state = decode(raw)?;
    if state["version"] == 3 {
        return Ok(());
    }
    let previous = previous.ok_or(
        "Recorded late private Deployment disappeared; explicit operator recovery is required",
    )?;
    let uid = state["deployment"]["uid"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(ERROR)?;
    if previous.metadata.uid.as_deref() != Some(uid)
        || previous.metadata.name.as_deref() != state["deployment"]["name"].as_str()
        || sandbox.metadata.uid.as_deref() != state["runtime"]["sandbox"]["uid"].as_str()
    {
        return Err(ERROR.into());
    }
    if state["phase"] == "Qualified" {
        return Ok(());
    }
    let workspace = sandbox.namespace().ok_or(ERROR)?;
    let name = sandbox.name_any();
    let current = object(client, &workspace, "KarsSandbox", "karssandboxes", &name).await?;
    let task = if let Some(expected) = state["runtime"].get("task") {
        let name = expected["object"]["name"].as_str().ok_or(ERROR)?;
        Some(object(client, &workspace, "KarsTask", "karstasks", name).await?)
    } else {
        None
    };
    let hold = intent(&state, &current, task.as_ref())?;
    if state["phase"] == "Restoring" {
        let secret = Api::<Secret>::namespaced(client.clone(), &namespace.name_any())
            .get(ADMIN)
            .await
            .map_err(|_| ERROR)?;
        rotated(&state, &secret, namespace, deployment)?;
    }
    if hold {
        deployment.spec.as_mut().ok_or(ERROR)?.replicas = Some(0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const RUNTIME_NS: &str = "/api/v1/namespaces/kars-runtime";
    const SOURCE: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karssandboxes/runtime";
    const TASK: &str = "/apis/kars.azure.com/v1alpha1/namespaces/work/karstasks/task";
    const DEPLOYMENT: &str = "/apis/apps/v1/namespaces/kars-runtime/deployments/runtime";
    const PRIVATE_SECRET: &str = "/api/v1/namespaces/kars-runtime/secrets/router-services-admin";

    #[derive(Default)]
    struct ApiState {
        objects: BTreeMap<String, Value>,
        writes: Vec<(String, Value)>,
        reads: Vec<String>,
    }

    impl ApiState {
        fn receipt(&self) -> Value {
            serde_json::from_str(
                self.objects[RUNTIME_NS]["metadata"]["annotations"][HISTORY]
                    .as_str()
                    .unwrap(),
            )
            .unwrap()
        }
        fn set_receipt(&mut self, receipt: Value) {
            self.objects.get_mut(RUNTIME_NS).unwrap()["metadata"]["annotations"][HISTORY] =
                json!(receipt.to_string());
        }
    }

    struct RuntimeApi {
        _server: MockServer,
        client: Client,
        state: Arc<Mutex<ApiState>>,
        sandbox: KarsSandbox,
        desired: Deployment,
    }

    impl RuntimeApi {
        async fn apply(&mut self) -> Result<bool, String> {
            crate::private_activation::apply_deployment(
                &self.client,
                &self.sandbox,
                &mut self.desired,
            )
            .await
        }

        async fn rejects_annotation(&mut self, raw: String) {
            self.state
                .lock()
                .unwrap()
                .objects
                .get_mut(RUNTIME_NS)
                .unwrap()["metadata"]["annotations"][HISTORY] = json!(raw);
            let before = self.state.lock().unwrap().objects.clone();
            assert!(self.apply().await.is_err());
            let state = self.state.lock().unwrap();
            assert!(state.reads.iter().any(|path| path == DEPLOYMENT));
            assert!(state.writes.is_empty());
            assert_eq!(state.objects, before);
        }
    }

    async fn runtime_api(
        phase: Option<&str>,
        previous_uid: Option<&str>,
        suspended: bool,
    ) -> RuntimeApi {
        let mut state = ApiState::default();
        let activation = crate::private_activation::test_support::install(
            &mut state.objects,
            "core",
            "core-uid",
            "controller",
            &[("kars-runtime", "runtime-ns")],
        );
        let mut task = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask",
            "metadata":{"name":"task","namespace":"work","uid":"task-uid","resourceVersion":"1","generation":1},
            "spec":{"objective":"Retire the owned runtime","envelope":{"tier":1,"authorityCeiling":1,"delegationDepth":0},"execution":{"launch":true}},
            "status":{"phase":"Ready","observedGeneration":1,"sandboxRef":{"name":"runtime"}}});
        let actual: crate::kars_task::KarsTask = serde_json::from_value(task.clone()).unwrap();
        let authorization = actual.envelope_digest();
        assert!(authorization.starts_with("sha256:"));
        task["status"]["envelopeDigest"] = json!(authorization);
        let sandbox = json!({"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsSandbox",
            "metadata":{"name":"runtime","namespace":"work","uid":"sandbox","resourceVersion":"2",
                "generation":if suspended {2} else {3},
                "annotations":{"kars.azure.com/namespace-uid":"runtime-ns"},
                "ownerReferences":[{"apiVersion":"kars.azure.com/v1alpha1","kind":"KarsTask","name":"task","uid":"task-uid","controller":true}]},
            "spec":{"inferenceRef":{"name":"policy"},"credentialsRef":{"name":"kars-credential-bundle-fixture","uid":"bundle-uid"},"suspended":suspended}});
        let namespace = state.objects.get_mut(RUNTIME_NS).unwrap();
        for (key, value) in [
            ("kars.azure.com/namespace-claim-version", "v1"),
            ("kars.azure.com/sandbox-name", "runtime"),
            ("kars.azure.com/sandbox-namespace", "work"),
            ("kars.azure.com/sandbox-uid", "sandbox"),
        ] {
            namespace["metadata"]["annotations"][key] = json!(value);
        }
        let epoch = namespace["metadata"]["annotations"][EPOCH].clone();
        namespace["metadata"]["annotations"]["kars.azure.com/private-parent-deployment"] =
            epoch.clone();
        namespace["metadata"]["annotations"]["kars.azure.com/private-parent-foreign"] =
            epoch.clone();
        let desired = json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"runtime","namespace":"kars-runtime"},
            "spec":{"replicas":1,"selector":{"matchLabels":{"app":"runtime"}},
                "template":{"metadata":{"annotations":{EPOCH:epoch,VERSION:"secret:2"}},
                    "spec":{"containers":[{"name":"inference-router","image":"fixture"}]}}}});
        let before = json!({"object":{"name":ADMIN,"uid":"secret","resourceVersion":"1"},
            "key":hash(&json!(STANDARD.encode(vec![b'A';64])))});
        let fresh = json!({"object":{"name":ADMIN,"uid":"secret","resourceVersion":"2"},
            "key":hash(&json!(STANDARD.encode(vec![b'B';64])))});
        if let Some(phase) = phase {
            let mut receipt = json!({"version":4,"phase":phase,
                "root":hash(&activation),"binding":"b".repeat(64),"attempt":"c".repeat(64),
                "runtime":{"sandbox":{"name":"runtime","uid":"sandbox","resourceVersion":"1"},"workspace":"work",
                    "spec":hash(&json!({"inferenceRef":{"name":"policy"},"credentialsRef":{"name":"kars-credential-bundle-fixture","uid":"bundle-uid"}})),
                    "owners":hash(&owners(&sandbox)),"generation":1,"suspended":false,
                    "task":{"object":{"name":"task","uid":"task-uid","resourceVersion":"1"},"generation":1,
                        "authorization":authorization,"spec":hash(&json!({"spec":task["spec"],"owners":owners(&task)}))}},
                "deployment":{"name":"runtime","uid":"deployment","resourceVersion":"1"},
                "structure":"d".repeat(64),"replicas":1,"captured":["old-pod"],"baseline":before,"epoch":epoch,
                "qualified":{"binding":"b".repeat(64),"template":"d".repeat(64),"material":fresh}});
            if phase == "v3" {
                receipt = json!({"version":3,"root":hash(&activation),
                    "binding":hash(&json!({"namespace":{"name":"kars-runtime","uid":"runtime-ns"},"consumers":[]})),
                    "phase":"Qualified","epoch":epoch});
            } else {
                if !["Restoring", "Qualified"].contains(&phase) {
                    receipt.as_object_mut().unwrap().remove("qualified");
                }
                if ["Pausing", "Retired"].contains(&phase) {
                    receipt.as_object_mut().unwrap().remove("epoch");
                }
            }
            namespace["metadata"]["annotations"][HISTORY] = json!(receipt.to_string());
        }
        state.objects.insert(SOURCE.into(), sandbox.clone());
        state.objects.insert(TASK.into(), task);
        state.objects.insert(PRIVATE_SECRET.into(), json!({
            "apiVersion":"v1","kind":"Secret","metadata":{"name":ADMIN,"namespace":"kars-runtime","uid":"secret","resourceVersion":"2",
                "annotations":{EPOCH:epoch}},"type":"Opaque","data":{"control-token":STANDARD.encode(vec![b'B';64])}
        }));
        if let Some(uid) = previous_uid {
            let mut existing = desired.clone();
            existing["metadata"]["uid"] = json!(uid);
            existing["metadata"]["resourceVersion"] = json!("7");
            existing["spec"]["replicas"] = json!(0);
            state.objects.insert(DEPLOYMENT.into(), existing);
        }
        let state = Arc::new(Mutex::new(state));
        let captured = state.clone();
        let server = MockServer::start().await;
        Mock::given(|_: &wiremock::Request| true).respond_with(move |request: &wiremock::Request| {
            let path = request.url.path();
            if request.method == "POST" && path.ends_with("/selfsubjectreviews") {
                return ResponseTemplate::new(201).set_body_json(json!({
                    "apiVersion":"authentication.k8s.io/v1","kind":"SelfSubjectReview",
                    "status":{"userInfo":{"username":"system:serviceaccount:core:kars-controller","uid":"controller"}}
                }));
            }
            let mut state = captured.lock().unwrap();
            if request.method == "GET" {
                state.reads.push(path.to_string());
                return state.objects.get(path).map_or_else(
                    || ResponseTemplate::new(404).set_body_json(json!({"apiVersion":"v1","kind":"Status",
                        "status":"Failure","reason":"NotFound","code":404,"message":"fixture object absent"})),
                    |value| ResponseTemplate::new(200).set_body_json(value),
                );
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            state.writes.push((format!("{} {path}", request.method), body.clone()));
            if path == DEPLOYMENT || path == DEPLOYMENT.strip_suffix("/runtime").unwrap() {
                let mut value = body;
                if request.method == "PATCH" {
                    let prior = &state.objects[DEPLOYMENT];
                    assert_eq!(value["metadata"]["uid"], prior["metadata"]["uid"]);
                    assert_eq!(value["metadata"]["resourceVersion"], prior["metadata"]["resourceVersion"]);
                } else {
                    assert_eq!(request.method, "POST");
                    assert!(!state.objects.contains_key(DEPLOYMENT));
                    value["metadata"]["uid"] = json!("created");
                }
                value["metadata"]["resourceVersion"] = json!("8");
                state.objects.insert(DEPLOYMENT.into(), value.clone());
                return ResponseTemplate::new(if request.method == "POST" {201} else {200}).set_body_json(value);
            }
            assert_eq!(request.method, "PATCH");
            assert_eq!(path, RUNTIME_NS);
            let namespace = state.objects.get_mut(RUNTIME_NS).unwrap();
            assert_eq!(body["metadata"]["uid"], namespace["metadata"]["uid"]);
            assert_eq!(body["metadata"]["resourceVersion"], namespace["metadata"]["resourceVersion"]);
            for (key, value) in body["metadata"]["annotations"].as_object().unwrap() {
                namespace["metadata"]["annotations"][key] = value.clone();
            }
            namespace["metadata"]["resourceVersion"] = json!("9");
            ResponseTemplate::new(200).set_body_json(namespace.clone())
        }).mount(&server).await;
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
        RuntimeApi {
            _server: server,
            client,
            state,
            sandbox: serde_json::from_value(sandbox).unwrap(),
            desired: serde_json::from_value(desired).unwrap(),
        }
    }

    #[tokio::test]
    async fn runtime_v4_rejects_missing_or_foreign_incarnations_in_every_phase_before_writes() {
        for phase in ["Pausing", "Retired", "Rotating", "Restoring", "Qualified"] {
            for fault in ["absent", "foreign", "unidentified", "missing-recorded-uid"] {
                let uid = match fault {
                    "absent" => None,
                    "foreign" => Some("foreign"),
                    _ => Some("deployment"),
                };
                let mut api = runtime_api(Some(phase), uid, false).await;
                {
                    let mut state = api.state.lock().unwrap();
                    if fault == "unidentified" {
                        state.objects.get_mut(DEPLOYMENT).unwrap()["metadata"]
                            .as_object_mut()
                            .unwrap()
                            .remove("uid");
                    }
                    if fault == "missing-recorded-uid" {
                        let mut receipt = state.receipt();
                        receipt["deployment"].as_object_mut().unwrap().remove("uid");
                        state.set_receipt(receipt);
                    }
                }
                let receipt = api.state.lock().unwrap().receipt();
                api.rejects_annotation(receipt.to_string()).await;
            }
        }
    }

    #[tokio::test]
    async fn runtime_rotating_deletion_and_concurrent_unsuspend_cannot_create_a_replacement() {
        let mut api = runtime_api(Some("Rotating"), Some("deployment"), true).await;
        assert_eq!(api.apply().await, Ok(true));
        assert_eq!(
            api.state.lock().unwrap().objects[DEPLOYMENT]["spec"]["replicas"],
            0
        );
        {
            let mut state = api.state.lock().unwrap();
            assert_eq!(state.writes.len(), 1);
            state.writes.clear();
            state.reads.clear();
            state.objects.remove(DEPLOYMENT);
            let sandbox = state.objects.get_mut(SOURCE).unwrap();
            sandbox["spec"]["suspended"] = json!(false);
            sandbox["metadata"]["generation"] = json!(3);
            sandbox["metadata"]["resourceVersion"] = json!("3");
            api.sandbox = serde_json::from_value(sandbox.clone()).unwrap();
        }
        api.desired.metadata.uid = None;
        api.desired.metadata.resource_version = None;
        api.desired.spec.as_mut().unwrap().replicas = Some(1);
        let before = api.state.lock().unwrap().objects.clone();
        let error = api.apply().await.unwrap_err();
        assert!(error.contains("disappeared"));
        let state = api.state.lock().unwrap();
        assert!(state.reads.iter().any(|path| path == DEPLOYMENT));
        assert!(state.writes.is_empty());
        assert_eq!(state.objects, before);
        assert!(!state.objects.contains_key(DEPLOYMENT));
    }

    #[tokio::test]
    async fn runtime_restore_checks_original_authority_and_new_key() {
        for fault in "none bare wrong uppercase trailing-newline source generation owner missing-task reused-key".split_whitespace() {
            let mut api = runtime_api(Some("Restoring"), Some("deployment"), false).await;
            {
                let mut state = api.state.lock().unwrap();
                let authorization = state.objects[TASK]["status"]["envelopeDigest"]
                    .as_str()
                    .unwrap()
                    .to_string();
                assert_eq!(
                    task_authorization(&json!(authorization)).unwrap(),
                    authorization
                );
                match fault {
                    "source" => {
                        state.objects.get_mut(SOURCE).unwrap()["spec"]["credentialsRef"]["name"] =
                            json!("changed")
                    }
                    "generation" => {
                        state.objects.get_mut(SOURCE).unwrap()["metadata"]["generation"] = json!(4)
                    }
                    "owner" => {
                        state.objects.get_mut(SOURCE).unwrap()["metadata"]["ownerReferences"] =
                            json!([])
                    }
                    "missing-task" => {
                        state.objects.remove(TASK);
                    }
                    "reused-key" => {
                        state.objects.get_mut(PRIVATE_SECRET).unwrap()["data"]["control-token"] =
                            json!(STANDARD.encode(vec![b'A'; 64]))
                    }
                    _ => {}
                }
                api.sandbox = serde_json::from_value(state.objects[SOURCE].clone()).unwrap();
                if ["bare", "wrong", "uppercase", "trailing-newline"].contains(&fault) {
                    let changed = match fault {
                        "bare" => authorization.trim_start_matches("sha256:").to_string(),
                        "wrong" => format!("sha256:{}", "f".repeat(64)),
                        "uppercase" => authorization.to_uppercase(),
                        _ => format!("{authorization}\n"),
                    };
                    state.objects.get_mut(TASK).unwrap()["status"]["envelopeDigest"] =
                        json!(changed);
                    if fault != "wrong" {
                        let mut receipt = state.receipt();
                        receipt["runtime"]["task"]["authorization"] = json!(changed);
                        state.set_receipt(receipt);
                    }
                }
            }
            let result = api.apply().await;
            let state = api.state.lock().unwrap();
            assert_eq!(
                state.reads.iter().any(|path| path == TASK),
                !["bare", "uppercase", "trailing-newline"].contains(&fault)
            );
            if fault == "none" {
                assert_eq!(result, Ok(true));
                assert_eq!(state.writes.len(), 1);
                assert_eq!(state.objects[DEPLOYMENT]["metadata"]["uid"], "deployment");
                assert_eq!(state.objects[DEPLOYMENT]["spec"]["replicas"], 1);
            } else {
                assert!(result.is_err(), "{fault}");
                assert!(state.writes.is_empty(), "{fault}");
                assert_eq!(state.objects[DEPLOYMENT]["spec"]["replicas"], 0);
            }
        }
    }

    #[tokio::test]
    async fn runtime_present_invalid_receipts_never_bypass_deleted_runtime_fences() {
        let values = [
            Value::Null,
            json!(false),
            json!(4),
            json!("4"),
            json!([]),
            json!({}),
            json!({"version":4}),
        ];
        for raw in values
            .iter()
            .map(Value::to_string)
            .chain(["{".into(), "".into()])
        {
            let mut api = runtime_api(Some("Rotating"), None, false).await;
            api.rejects_annotation(raw).await;
        }
        let versions = json!([null, true, "4", -1, 0, 1, 2, 3, 5, 4.0]);
        for version in versions.as_array().unwrap() {
            let mut api = runtime_api(Some("Rotating"), None, false).await;
            let mut receipt = api.state.lock().unwrap().receipt();
            receipt["version"] = version.clone();
            api.rejects_annotation(receipt.to_string()).await;
        }
    }

    #[tokio::test]
    async fn runtime_v4_requires_complete_typed_receipts_even_with_a_matching_deployment() {
        for pointer in "/root /binding /runtime/spec /runtime/generation /runtime/task/authorization /epoch /baseline/key /captured /qualified/template".split_whitespace() {
            let mut api = runtime_api(Some("Qualified"), Some("deployment"), false).await;
            let mut receipt = api.state.lock().unwrap().receipt();
            *receipt.pointer_mut(pointer).unwrap() = Value::Null;
            api.rejects_annotation(receipt.to_string()).await;
        }
    }

    #[tokio::test]
    async fn runtime_without_v4_preserves_private_create_and_unqualified_core_dispatch() {
        for phase in [None, Some("v3")] {
            let mut api = runtime_api(phase, None, false).await;
            assert_eq!(api.apply().await, Ok(true));
            let state = api.state.lock().unwrap();
            assert!(
                state
                    .writes
                    .iter()
                    .any(|(path, _)| path.starts_with("POST ") && path.ends_with("/deployments"))
            );
            assert_eq!(state.objects[DEPLOYMENT]["metadata"]["uid"], "created");
            assert_eq!(state.objects[DEPLOYMENT]["spec"]["replicas"], 1);
            assert_eq!(
                state.objects[RUNTIME_NS]["metadata"]["annotations"]["kars.azure.com/private-parent-created"],
                "a".repeat(64)
            );
        }
        let mut api = runtime_api(None, None, false).await;
        api.desired
            .spec
            .as_mut()
            .unwrap()
            .template
            .metadata
            .as_mut()
            .unwrap()
            .annotations
            .as_mut()
            .unwrap()
            .remove(EPOCH);
        assert_eq!(api.apply().await, Ok(false));
        let state = api.state.lock().unwrap();
        assert!(state.reads.is_empty());
        assert!(state.writes.is_empty());
    }
}
