use super::mission_records::{
    mission_evidence_key, mission_output_candidate, project_mission_output_record,
    select_mission_evidence_records, select_mission_output_records, trace_record_identity,
};
use super::providers::{
    classify_provider, image_registry_host, normalize_registry_host, public_registry,
};
use super::sandboxes::descendant_sandbox_objects;
use k8s_openapi::api::core::v1::ConfigMap;
use kube::api::DynamicObject;
use serde_json::json;
use std::collections::BTreeMap;

fn id(r: Option<(String, String, String)>) -> Option<String> {
    r.map(|(i, _, _)| i)
}

#[test]
fn descendant_sandboxes_include_nested_agents_once() {
    let sandbox = |name: &str, parent: Option<&str>| -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "kars.azure.com/v1alpha1",
            "kind": "KarsSandbox",
            "metadata": {
                "name": name,
                "labels": parent.map(|parent| json!({"kars.azure.com/parent": parent}))
            }
        }))
        .expect("sandbox")
    };
    let items = vec![
        sandbox("child", Some("root")),
        sandbox("grandchild", Some("child")),
        sandbox("unrelated", Some("other")),
    ];

    let names = descendant_sandbox_objects(&items, "root")
        .into_iter()
        .filter_map(|sandbox| sandbox.metadata.name)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        names,
        std::collections::HashSet::from(["child".to_string(), "grandchild".to_string(),])
    );
}

#[test]
fn registry_matching_covers_private_runtime_images() {
    assert_eq!(
        image_registry_host("example.azurecr.io/kars-runtime-hermes:latest"),
        "example.azurecr.io"
    );
    assert_eq!(
        normalize_registry_host("https://example.azurecr.io/v1/"),
        "example.azurecr.io"
    );
    assert!(!public_registry("example.azurecr.io"));
    assert!(public_registry("mcr.microsoft.com"));
}

#[test]
fn mission_evidence_annotation_restores_long_nonce_identity() {
    let full = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let mut config_map = ConfigMap::default();
    config_map.metadata.annotations = Some(BTreeMap::from([(
        "kars.azure.com/mission-evidence-key".to_string(),
        full.to_string(),
    )]));
    config_map.metadata.labels = Some(BTreeMap::from([(
        "kars.azure.com/mission-output".to_string(),
        "stock-monitor-persistent-qual-principal-assig-0123456789ab".to_string(),
    )]));

    assert_eq!(
        mission_evidence_key(&config_map, "kars.azure.com/mission-output").as_deref(),
        Some(full)
    );
}

#[test]
fn mission_evidence_label_remains_legacy_fallback() {
    let mut config_map = ConfigMap::default();
    config_map.metadata.labels = Some(BTreeMap::from([(
        "kars.azure.com/mission-output".to_string(),
        "team-run-100".to_string(),
    )]));

    assert_eq!(
        mission_evidence_key(&config_map, "kars.azure.com/mission-output").as_deref(),
        Some("team-run-100")
    );
}

#[test]
fn legacy_principal_label_restores_stable_task_name() {
    let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let mut config_map = ConfigMap::default();
    config_map.metadata.labels = Some(BTreeMap::from([
        (
            "kars.azure.com/mission-output".to_string(),
            nonce.to_string(),
        ),
        (
            "kars.azure.com/mission-principal".to_string(),
            "stock-monitor-persistent-qual-principal".to_string(),
        ),
    ]));
    config_map.data = Some(BTreeMap::from([(
        "assignmentNonce".to_string(),
        nonce.to_string(),
    )]));

    let (_, _, data) = mission_output_candidate(config_map).expect("candidate");
    assert_eq!(
        data.get("taskName").map(String::as_str),
        Some("stock-monitor-persistent-qual-principal")
    );
}

#[test]
fn ordinary_mission_enumeration_keeps_the_task_pointer() {
    let first_nonce = "run-1784912062312097896";
    let latest_nonce = "run-1784915840189332732";
    let first = BTreeMap::from([
        ("assignmentNonce".to_string(), first_nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let latest = BTreeMap::from([
        ("assignmentNonce".to_string(), latest_nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let selected = select_mission_output_records(vec![
        (first_nonce.to_string(), Some("archive".to_string()), first),
        (
            latest_nonce.to_string(),
            Some("archive".to_string()),
            latest.clone(),
        ),
        (
            "kompli-research".to_string(),
            Some("current".to_string()),
            latest,
        ),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "kompli-research");
}

#[test]
fn explicit_current_pointer_beats_legacy_archive_for_same_task() {
    let old_nonce = "rev-1";
    let latest_nonce = "rev-2";
    let legacy = BTreeMap::from([
        ("assignmentNonce".to_string(), old_nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let current = BTreeMap::from([
        ("assignmentNonce".to_string(), latest_nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let selected = select_mission_output_records(vec![
        (old_nonce.to_string(), None, legacy),
        (
            "kompli-research".to_string(),
            Some("current".to_string()),
            current,
        ),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "kompli-research");
}

#[test]
fn legacy_ordinary_run_archives_do_not_become_phantom_tasks() {
    let old_nonce = "run-1784912062312097896";
    let latest_nonce = "run-1784915840189332732";
    let mut archive = ConfigMap::default();
    archive.metadata.labels = Some(BTreeMap::from([
        (
            "kars.azure.com/mission-output".to_string(),
            old_nonce.to_string(),
        ),
        (
            "kars.azure.com/mission-principal".to_string(),
            "kompli-research".to_string(),
        ),
    ]));
    archive.data = Some(BTreeMap::from([(
        "assignmentNonce".to_string(),
        old_nonce.to_string(),
    )]));
    let mut current = ConfigMap::default();
    current.metadata.labels = Some(BTreeMap::from([(
        "kars.azure.com/mission-output".to_string(),
        "kompli-research".to_string(),
    )]));
    current.data = Some(BTreeMap::from([(
        "assignmentNonce".to_string(),
        latest_nonce.to_string(),
    )]));
    let selected = select_mission_output_records(vec![
        mission_output_candidate(archive).expect("archive"),
        mission_output_candidate(current).expect("current"),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "kompli-research");
}

#[test]
fn persistent_team_latest_enumeration_keeps_the_current_pointer() {
    let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let data = BTreeMap::from([
        ("assignmentNonce".to_string(), nonce.to_string()),
        (
            "taskName".to_string(),
            "stock-monitor-persistent-qual-principal".to_string(),
        ),
        (
            "team".to_string(),
            "stock-monitor-persistent-qual".to_string(),
        ),
    ]);
    let selected = select_mission_output_records(vec![
        (nonce.to_string(), Some("archive".to_string()), data.clone()),
        (
            "stock-monitor-persistent-qual-principal".to_string(),
            Some("current".to_string()),
            data,
        ),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, "stock-monitor-persistent-qual-principal");
}

#[test]
fn accounting_enumeration_keeps_archives_and_drops_current_pointers() {
    let nonce = "run-1784915840189332732";
    let data = BTreeMap::from([
        ("assignmentNonce".to_string(), nonce.to_string()),
        ("taskName".to_string(), "kompli-research".to_string()),
    ]);
    let selected = select_mission_evidence_records(vec![
        (nonce.to_string(), Some("archive".to_string()), data.clone()),
        (
            "kompli-research".to_string(),
            Some("current".to_string()),
            data,
        ),
    ]);

    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0, nonce);
}

#[test]
fn accounting_enumeration_keeps_each_rerun_archive() {
    let first_nonce = "rev-1";
    let latest_nonce = "rev-2";
    let selected = select_mission_evidence_records(vec![
        (
            first_nonce.to_string(),
            Some("archive".to_string()),
            BTreeMap::from([("assignmentNonce".to_string(), first_nonce.to_string())]),
        ),
        (
            latest_nonce.to_string(),
            Some("archive".to_string()),
            BTreeMap::from([("assignmentNonce".to_string(), latest_nonce.to_string())]),
        ),
        (
            "kompli-research".to_string(),
            Some("current".to_string()),
            BTreeMap::from([("assignmentNonce".to_string(), latest_nonce.to_string())]),
        ),
    ]);

    assert_eq!(selected.len(), 2);
    assert!(selected.iter().any(|(key, _)| key == first_nonce));
    assert!(selected.iter().any(|(key, _)| key == latest_nonce));
}

#[test]
fn persistent_archive_projects_stable_task_and_separate_evidence_key() {
    let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let data = BTreeMap::from([
        ("assignmentNonce".to_string(), nonce.to_string()),
        (
            "taskName".to_string(),
            "stock-monitor-persistent-qual-principal".to_string(),
        ),
        (
            "team".to_string(),
            "stock-monitor-persistent-qual".to_string(),
        ),
    ]);
    let projected = project_mission_output_record(nonce.to_string(), data);

    assert_eq!(
        projected.task_name,
        "stock-monitor-persistent-qual-principal"
    );
    assert_eq!(projected.evidence_key, nonce);
}

#[test]
fn mirrored_trace_records_share_one_counting_identity() {
    let nonce = "stock-monitor-persistent-qual-principal-assign-1784912607085271247";
    let data = BTreeMap::from([
        ("assignmentNonce".to_string(), nonce.to_string()),
        (
            "trace.json".to_string(),
            r#"[{"kind":"round"}]"#.to_string(),
        ),
        ("capturedAt".to_string(), "2026-07-24T19:00:00Z".to_string()),
    ]);
    let mut archive = ConfigMap::default();
    archive.metadata.name = Some(format!("kars-mission-trace-{nonce}"));
    archive.metadata.annotations = Some(BTreeMap::from([(
        "kars.azure.com/mission-evidence-role".to_string(),
        "archive".to_string(),
    )]));
    archive.data = Some(data.clone());
    let mut current = ConfigMap::default();
    current.metadata.name =
        Some("kars-mission-trace-stock-monitor-persistent-qual-principal".to_string());
    current.metadata.annotations = Some(BTreeMap::from([(
        "kars.azure.com/mission-evidence-role".to_string(),
        "current".to_string(),
    )]));
    current.data = Some(data);

    assert!(trace_record_identity(&archive).is_some());
    assert!(trace_record_identity(&current).is_none());
}

#[test]
fn explicit_override_wins() {
    let eps = vec!["https://models.github.ai/inference".to_string()];
    assert_eq!(
        id(classify_provider(Some("github-copilot"), &eps, None)).as_deref(),
        Some("github-copilot")
    );
    assert_eq!(
        id(classify_provider(
            Some("github-models"),
            &eps,
            Some("gho_x")
        ))
        .as_deref(),
        Some("github-models")
    );
    assert_eq!(
        id(classify_provider(Some("foundry"), &[], None)).as_deref(),
        Some("azure-foundry")
    );
}

#[test]
fn github_endpoint_with_oauth_token_is_copilot() {
    // The real localkarstest shape: models.github.ai + a gho_ OAuth token.
    let eps = vec!["https://models.github.ai/inference".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, Some("gho_"))).as_deref(),
        Some("github-copilot")
    );
    assert_eq!(
        id(classify_provider(None, &eps, Some("ghu_"))).as_deref(),
        Some("github-copilot")
    );
}

#[test]
fn github_endpoint_with_pat_is_models() {
    let eps = vec!["https://models.github.ai/inference".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, Some("ghp_"))).as_deref(),
        Some("github-models")
    );
    assert_eq!(
        id(classify_provider(None, &eps, None)).as_deref(),
        Some("github-models")
    );
}

#[test]
fn copilot_endpoint_is_copilot() {
    let eps = vec!["https://api.githubcopilot.com".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, None)).as_deref(),
        Some("github-copilot")
    );
}

#[test]
fn foundry_endpoint_and_empty() {
    let eps = vec!["https://my-proj.openai.azure.com".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, None)).as_deref(),
        Some("azure-foundry")
    );
    assert_eq!(id(classify_provider(None, &[], None)), None);
}

#[test]
fn local_inference_endpoint_is_not_mislabeled_as_foundry() {
    // A promoted local model's endpoint is always a Service DNS name in
    // the Bridge-owned kars-local-inference namespace — must be labeled
    // distinctly, not fall into the generic Foundry bucket every other
    // unrecognized endpoint gets.
    let eps = vec!["http://my-model.kars-local-inference.svc.cluster.local:80".to_string()];
    assert_eq!(
        id(classify_provider(None, &eps, None)).as_deref(),
        Some("local-inference")
    );
}
