// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Default)]
struct State {
    lease: Option<Lease>,
    writes: usize,
    attempts: usize,
    reject_writes: bool,
    delay_reads: bool,
    delay_writes: bool,
}

#[derive(Clone)]
struct LeaseServer(Arc<Mutex<State>>);

fn failure(code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({
        "apiVersion": "v1", "kind": "Status", "status": "Failure",
        "reason": if code == 404 { "NotFound" } else { "Conflict" }, "code": code,
        "message": "lease request rejected",
    }))
}

impl Respond for LeaseServer {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = self.0.lock().unwrap();
        match request.method.as_str() {
            "GET" => {
                let response = state.lease.as_ref().map_or_else(
                    || failure(404),
                    |lease| ResponseTemplate::new(200).set_body_json(lease),
                );
                if state.delay_reads {
                    response.set_delay(Duration::from_secs(5))
                } else {
                    response
                }
            }
            "POST" | "PATCH" => {
                state.attempts += 1;
                if state.reject_writes {
                    return failure(409);
                }
                let incoming: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                let mut lease = if request.method.as_str() == "POST" {
                    if state.lease.is_some() {
                        return failure(409);
                    }
                    let mut lease: Lease = serde_json::from_value(incoming.clone()).unwrap();
                    lease.metadata.uid = Some("lease-uid".into());
                    lease
                } else {
                    let Some(current) = state.lease.as_ref() else {
                        return failure(404);
                    };
                    if incoming["metadata"]["uid"] != json!(current.metadata.uid)
                        || incoming["metadata"]["resourceVersion"]
                            != json!(current.metadata.resource_version)
                    {
                        return failure(409);
                    }
                    current.clone()
                };
                lease.spec = Some(serde_json::from_value(incoming["spec"].clone()).unwrap());
                state.writes += 1;
                lease.metadata.resource_version = Some(format!("write-{}", state.writes));
                let response = ResponseTemplate::new(200).set_body_json(&lease);
                state.lease = Some(lease);
                if state.delay_writes {
                    response.set_delay(Duration::from_secs(5))
                } else {
                    response
                }
            }
            _ => panic!("unexpected lease operation"),
        }
    }
}

fn config(identity: &str) -> LeaderElectionConfig {
    LeaderElectionConfig {
        lease_name: DEFAULT_LEASE_NAME.into(),
        namespace: "kars-system".into(),
        identity: identity.into(),
        lease_duration_secs: 3,
        renew_period_secs: 1,
    }
}

fn existing(holder: &str) -> Lease {
    serde_json::from_value(json!({
        "apiVersion": "coordination.k8s.io/v1", "kind": "Lease",
        "metadata": {"name": DEFAULT_LEASE_NAME, "namespace": "kars-system",
                     "uid": "lease-uid", "resourceVersion": "observed"},
        "spec": {"holderIdentity": holder, "leaseDurationSeconds": 15,
                 "renewTime": Utc::now().to_rfc3339()},
    }))
    .unwrap()
}

async fn setup(state: State) -> (MockServer, Client, Arc<Mutex<State>>) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let server = MockServer::start().await;
    let state = Arc::new(Mutex::new(state));
    Mock::given(wiremock::matchers::any())
        .respond_with(LeaseServer(state.clone()))
        .mount(&server)
        .await;
    let client = Client::try_from(kube::Config::new(server.uri().parse().unwrap())).unwrap();
    (server, client, state)
}

#[tokio::test]
async fn contenders_cannot_both_write_the_same_observed_lease() {
    for absent in [false, true] {
        let observed = (!absent).then(|| existing("old"));
        let (_server, client, state) = setup(State {
            lease: observed.clone(),
            ..Default::default()
        })
        .await;
        let leases = Api::namespaced(client, "kars-system");
        let first = config("first");
        let second = config("second");
        let (a, b) = tokio::join!(
            patch_or_create_lease(&leases, &first, Utc::now(), observed.as_ref()),
            patch_or_create_lease(&leases, &second, Utc::now(), observed.as_ref()),
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let state = state.lock().unwrap();
        assert_eq!(state.writes, 1);
        assert_eq!(state.attempts, 2);
    }
}

#[tokio::test]
async fn replacement_uid_cannot_be_overwritten_even_with_the_same_version() {
    let observed = existing("old");
    let mut replacement = observed.clone();
    replacement.metadata.uid = Some("replacement".into());
    let (_server, client, state) = setup(State {
        lease: Some(replacement),
        ..Default::default()
    })
    .await;
    let leases = Api::namespaced(client, "kars-system");
    assert!(
        patch_or_create_lease(&leases, &config("us"), Utc::now(), Some(&observed))
            .await
            .is_err()
    );
    assert_eq!(state.lock().unwrap().writes, 0);
}

#[tokio::test]
async fn failed_initial_create_or_renew_never_signals_readiness() {
    for lease in [None, Some(existing("us"))] {
        let (_server, client, state) = setup(State {
            lease,
            reject_writes: true,
            ..Default::default()
        })
        .await;
        let (tx, mut rx) = oneshot::channel();
        let task = tokio::spawn(acquire_and_hold(client, config("us"), tx));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut rx)
                .await
                .is_err()
        );
        assert!(state.lock().unwrap().attempts > 0);
        assert_eq!(state.lock().unwrap().writes, 0);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test]
async fn a_successful_holder_renews_without_restarting_its_bundle() {
    let (_server, client, state) = setup(State::default()).await;
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(acquire_and_hold(client, config("us"), tx));
    tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while state.lock().unwrap().writes < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn holder_fail_stops_on_loss_deletion_replacement_and_expiration() {
    for change in ["owner", "deletion", "replacement", "expiration"] {
        let (_server, client, state) = setup(State::default()).await;
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(acquire_and_hold(client, config("us"), tx));
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            match change {
                "owner" => {
                    state
                        .lease
                        .as_mut()
                        .unwrap()
                        .spec
                        .as_mut()
                        .unwrap()
                        .holder_identity = Some("other".into())
                }
                "deletion" => state.lease = None,
                "replacement" => state.lease.as_mut().unwrap().metadata.uid = Some("new".into()),
                "expiration" => {
                    state
                        .lease
                        .as_mut()
                        .unwrap()
                        .spec
                        .as_mut()
                        .unwrap()
                        .renew_time = None
                }
                _ => unreachable!(),
            }
        }
        let error = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            error.to_string().contains("controller lease"),
            "{change}: {error}"
        );
        assert_eq!(state.lock().unwrap().writes, 1, "{change}");
    }
}

#[tokio::test]
async fn hung_get_or_write_cannot_outlive_the_renewal_deadline() {
    for delay_reads in [true, false] {
        let (_server, client, state) = setup(State::default()).await;
        let (tx, rx) = oneshot::channel();
        let started = Instant::now();
        let task = tokio::spawn(acquire_and_hold(client, config("us"), tx));
        tokio::time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            state.delay_reads = delay_reads;
            state.delay_writes = !delay_reads;
        }
        let error = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(format!("{error:#}").contains("deadline"), "{error:#}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
