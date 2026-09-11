use super::BlueprintDto;
use super::ExecutionPhaseDto;
use super::ExecutionPlanDto;
use super::ExecutionRoleDto;
use super::ExecutionSynthesisDto;
use super::MissionArtifactDto;
use super::MissionResultDto;
use super::ModelDto;
use super::TaskAssignmentEventDto;
use super::TeamCollaborationEventDto;
use super::clean_objective;
use super::diagnostics::diagnose_run_failure;
use super::egress::normalize_ttl;
use super::evidence::{
    ARTIFACT_PREVIEW_MAX_BYTES, ARTIFACT_PREVIEW_TOTAL_BYTES, artifact_preview,
    canonicalize_assignment_event_roles, merge_trace_total_tokens, select_task_checkpoint,
    structured_team_evidence, subagent_trace_from_artifacts, valid_task_checkpoint,
};
use super::mapping::to_sub_agent;
use super::presentation::deliverable_pull_requests;

#[test]
fn execution_plan_dto_into_crd_preserves_web_search_capability() {
    let blueprint = BlueprintDto {
        execution_plan: Some(ExecutionPlanDto {
            schema: "kars.execution-plan/v1".into(),
            roles: vec![ExecutionRoleDto {
                name: "source-scout".into(),
                objective: "Discover exact URLs and fetch the evidence.".into(),
                depends_on: Vec::new(),
                phases: vec![ExecutionPhaseDto {
                    name: "discover".into(),
                    objective: "Search and fetch the exact URLs.".into(),
                    capabilities: vec!["web-search".into(), "network".into()],
                    required_tool_calls: Vec::new(),
                    min_tool_calls: 1,
                    max_tool_calls: 4,
                    fresh_context: true,
                }],
                budget_tokens: None,
            }],
            max_parallel: 1,
            synthesis: ExecutionSynthesisDto {
                objective: "Return the verified answer.".into(),
                capabilities: Vec::new(),
                max_tool_calls: 0,
            },
            deliverables: Vec::new(),
        }),
        ..Default::default()
    };

    let crd = blueprint.into_crd();
    assert_eq!(
        crd.execution_plan.expect("execution plan").roles[0].phases[0].capabilities,
        vec!["web-search".to_string(), "network".to_string()]
    );
}

#[test]
fn blueprint_dto_preserves_ordered_model_fallbacks() {
    let blueprint = BlueprintDto {
        model: Some(ModelDto {
            provider: "local-inference".into(),
            deployment: "gpt-oss-120b".into(),
        }),
        model_fallbacks: vec![
            ModelDto {
                provider: "github-copilot".into(),
                deployment: "gpt-5.6-sol".into(),
            },
            ModelDto {
                provider: "foundry".into(),
                deployment: "gpt-5.4-pro".into(),
            },
        ],
        ..Default::default()
    };

    let crd = blueprint.into_crd();
    assert_eq!(crd.model_fallbacks.len(), 2);
    assert_eq!(crd.model_fallbacks[0].provider, "github-copilot");
    assert_eq!(crd.model_fallbacks[1].deployment, "gpt-5.4-pro");
}

#[test]
fn schema_rejection_is_diagnosed_as_router_model_compatibility() {
    let logs = vec![
        "rawError=400 Unknown parameter: 'stream_options.include_usage'".to_string(),
        "LLM request failed: provider rejected the request schema or tool payload.".to_string(),
    ];
    let (cause, remedy, harness_issue, evidence) =
        diagnose_run_failure(&logs, &[], Some("provider rejected the request schema"));
    assert!(cause.contains("translated inference request"));
    assert!(remedy.contains("corrected inference router"));
    assert!(!harness_issue);
    assert_eq!(evidence.len(), 1);
}

#[test]
fn completed_subagent_trace_is_rehydrated_from_artifact() {
    let artifacts = vec![MissionArtifactDto {
        name: "artifacts/.run-x/subagent-telemetry.jsonl".into(),
        size_bytes: None,
        content: Some(
            r#"{"at":"2026-07-23T12:00:00Z","event":"subagent_trace","member":"ci-verifier","trace":{"kind":"tool","name":"github_checks","ok":true}}"#
                .into(),
        ),
        content_bytes: None,
        content_truncated: false,
        source_agent: None,
        source_path: None,
        digest: None,
        full_content: None,
    }];

    let events = subagent_trace_from_artifacts(&artifacts);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["agent"], "ci-verifier");
    assert_eq!(events[0]["agentRole"], "subagent");
    assert_eq!(events[0]["ts"], "2026-07-23T12:00:00Z");
    assert_eq!(events[0]["name"], "github_checks");
}

#[test]
fn artifact_preview_is_bounded_but_full_content_remains_internal() {
    let mut budget = ARTIFACT_PREVIEW_TOTAL_BYTES;
    let full = "x".repeat(ARTIFACT_PREVIEW_MAX_BYTES + 100);
    let (preview, bytes, truncated, internal) = artifact_preview(Some(full.clone()), &mut budget);

    assert_eq!(
        preview.as_ref().map(String::len),
        Some(ARTIFACT_PREVIEW_MAX_BYTES)
    );
    assert_eq!(bytes, Some(full.len() as i64));
    assert!(truncated);
    assert_eq!(internal.as_deref(), Some(full.as_str()));
}

#[test]
fn artifact_previews_share_a_bounded_response_budget() {
    let mut budget = ARTIFACT_PREVIEW_MAX_BYTES + 100;
    let first = "a".repeat(ARTIFACT_PREVIEW_MAX_BYTES + 1);
    let second = "b".repeat(ARTIFACT_PREVIEW_MAX_BYTES);

    let (first_preview, _, first_truncated, _) = artifact_preview(Some(first), &mut budget);
    let (second_preview, _, second_truncated, _) = artifact_preview(Some(second), &mut budget);

    assert_eq!(
        first_preview.as_ref().map(String::len),
        Some(ARTIFACT_PREVIEW_MAX_BYTES)
    );
    assert_eq!(second_preview.as_ref().map(String::len), Some(100));
    assert!(first_truncated);
    assert!(second_truncated);
    assert_eq!(budget, 0);
}

#[test]
fn empty_artifact_is_not_reported_as_truncated() {
    let mut budget = ARTIFACT_PREVIEW_TOTAL_BYTES;
    let (preview, bytes, truncated, internal) = artifact_preview(Some(String::new()), &mut budget);

    assert_eq!(preview.as_deref(), Some(""));
    assert_eq!(bytes, Some(0));
    assert!(!truncated);
    assert_eq!(internal.as_deref(), Some(""));
}

#[test]
fn artifact_preview_respects_utf8_boundaries_and_exhausted_budget() {
    let mut budget = 5;
    let (preview, bytes, truncated, _) =
        artifact_preview(Some("abcd\u{1f642}".to_string()), &mut budget);

    assert_eq!(preview.as_deref(), Some("abcd"));
    assert_eq!(bytes, Some(8));
    assert!(truncated);
    assert_eq!(budget, 1);

    budget = 0;
    let (preview, bytes, truncated, _) =
        artifact_preview(Some("still here".to_string()), &mut budget);
    assert!(preview.is_none());
    assert_eq!(bytes, Some(10));
    assert!(truncated);
}

#[test]
fn full_artifact_content_is_private_but_available_for_trace_recovery() {
    let telemetry = r#"{"at":"2026-07-23T12:00:00Z","event":"subagent_trace","member":"ci-verifier","trace":{"kind":"tool","name":"github_checks","ok":true}}"#;
    let artifact = MissionArtifactDto {
        name: "artifacts/.run-x/subagent-telemetry.jsonl".into(),
        size_bytes: Some(telemetry.len() as i64),
        content: Some("{\"at\":\"2026".into()),
        content_bytes: Some(telemetry.len() as i64),
        content_truncated: true,
        source_agent: None,
        source_path: None,
        digest: None,
        full_content: Some(telemetry.into()),
    };

    let serialized = serde_json::to_value(&artifact).expect("serialize artifact preview");
    assert_eq!(serialized["content"], "{\"at\":\"2026");
    assert!(serialized.get("full_content").is_none());

    let events = subagent_trace_from_artifacts(&[artifact]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["name"], "github_checks");
}

#[test]
fn structured_team_evidence_uses_full_content_not_preview_order() {
    let role_plan = MissionArtifactDto {
        name: "role-plan.json".into(),
        size_bytes: None,
        content: None,
        content_bytes: Some(91),
        content_truncated: true,
        source_agent: None,
        source_path: None,
        digest: None,
        full_content: Some(
            r#"{"selected_roles":[{"role":"builder"}],"skipped_roles":["observer"]}"#.into(),
        ),
    };
    let collaboration = MissionArtifactDto {
        name: "collaboration.jsonl".into(),
        size_bytes: None,
        content: Some("{\"at\":\"truncated".into()),
        content_bytes: Some(200),
        content_truncated: true,
        source_agent: None,
        source_path: None,
        digest: None,
        full_content: Some(
            r#"{"at":"2026-07-23T12:00:00Z","event":"child_handback","from_agent":"builder","outcome":"success","reply_preview":"done"}"#
                .into(),
        ),
    };

    let (plan, events) = structured_team_evidence(&[role_plan, collaboration]);

    assert_eq!(plan.selected_roles, ["builder"]);
    assert_eq!(plan.skipped_roles, ["observer"]);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].member.as_deref(), Some("builder"));
    assert_eq!(events[0].event, "child_handback");
}

#[test]
fn assignment_ledger_uses_canonical_role_from_assignment_message() {
    let mut events = vec![TaskAssignmentEventDto {
        sequence: 1,
        event_id: "event-1".into(),
        task_id: "run-1".into(),
        event_type: "child_progress".into(),
        state: "Completed".into(),
        at: "2026-08-04T21:36:50Z".into(),
        worker_did: None,
        stage: Some("child_handback".into()),
        child_task_id: Some("message-1".into()),
        child_role: Some("principal-remediatio-b9220dcf".into()),
        outcome: Some("success".into()),
        message: None,
    }];
    let collaboration = vec![TeamCollaborationEventDto {
        at: Some("2026-08-04T21:35:32Z".into()),
        event: "assignment_sent".into(),
        agent: Some("principal".into()),
        member: Some("remediation-engineer".into()),
        outcome: None,
        message_id: Some("message-1".into()),
        reply_preview: None,
        content_preview: None,
    }];

    canonicalize_assignment_event_roles(&mut events, &collaboration);

    assert_eq!(
        events[0].child_role.as_deref(),
        Some("remediation-engineer")
    );
}

#[test]
fn successful_result_hides_stale_bootstrap_checkpoint() {
    let progress = serde_json::json!({
        "schema": "kars.checkpoint/v1",
        "milestone_id": "dependency-pr",
        "status": "in_progress",
        "summary": "Controller initialized the durable milestone checkpoint."
    });

    assert!(select_task_checkpoint(Some(progress.clone()), &[], true).is_none());
    assert!(select_task_checkpoint(Some(progress), &[], false).is_some());
}

#[test]
fn completed_artifact_checkpoint_wins_over_bootstrap_progress() {
    let completed = r#"{
        "schema": "kars.checkpoint/v1",
        "milestone_id": "dependency-pr",
        "status": "completed",
        "summary": "All required handbacks were retained."
    }"#;
    let artifact = MissionArtifactDto {
        name: "task-checkpoint.json".into(),
        size_bytes: Some(completed.len() as i64),
        content: None,
        content_bytes: Some(completed.len() as i64),
        content_truncated: true,
        source_agent: None,
        source_path: None,
        digest: None,
        full_content: Some(completed.into()),
    };
    let progress = serde_json::json!({
        "schema": "kars.checkpoint/v1",
        "milestone_id": "dependency-pr",
        "status": "in_progress",
        "summary": "Controller initialized the durable milestone checkpoint."
    });

    let checkpoint = select_task_checkpoint(Some(progress), &[artifact], true).expect("checkpoint");

    assert_eq!(checkpoint["status"], "completed");
}

#[test]
fn aggregate_trace_tokens_replace_principal_only_total() {
    let mut result = Some(MissionResultDto {
        output: "done".into(),
        status: Some("ok".into()),
        model: None,
        total_tokens: Some(8_505),
        prompt_tokens: None,
        completion_tokens: None,
        finished_at: None,
        assignment_nonce: None,
        source: None,
        blocked: None,
        artifact_persistence: None,
        artifact_count: None,
        declared_artifact_count: None,
    });

    merge_trace_total_tokens(&mut result, 22_016);

    assert_eq!(result.and_then(|value| value.total_tokens), Some(22_016));
}

#[test]
fn subagent_projects_human_identity_and_runtime_metadata() {
    let object: kube::core::DynamicObject = serde_json::from_value(serde_json::json!({
        "apiVersion": "kars.azure.com/v1alpha1",
        "kind": "KarsSandbox",
        "metadata": {
            "name": "researcher-run-7f4c",
            "namespace": "kars-team",
            "labels": {
                "kars.azure.com/role": "Research specialist",
                "kars.azure.com/parent": "principal-run"
            },
            "annotations": {
                "kars.azure.com/logical-agent-id": "researcher",
                "kars.azure.com/model": "gpt-5.4"
            }
        },
        "spec": {
            "runtime": {
                "kind": "openclaw"
            }
        },
        "status": {
            "phase": "Running"
        }
    }))
    .expect("deserialize KarsSandbox");

    let dto = to_sub_agent(&object);

    assert_eq!(dto.name, "researcher-run-7f4c");
    assert_eq!(dto.namespace, "kars-team");
    assert_eq!(dto.phase.as_deref(), Some("Running"));
    assert_eq!(dto.runtime.as_deref(), Some("openclaw"));
    assert_eq!(dto.role.as_deref(), Some("Research specialist"));
    assert_eq!(dto.parent.as_deref(), Some("principal-run"));
    assert_eq!(dto.logical_agent_id.as_deref(), Some("researcher"));
    assert_eq!(dto.model.as_deref(), Some("gpt-5.4"));
}

#[test]
fn malformed_checkpoint_is_not_exposed_to_the_ui() {
    assert!(
        valid_task_checkpoint(serde_json::json!({
            "schema": "kars.checkpoint/v1",
            "status": "completed"
        }))
        .is_none()
    );
    assert!(
        valid_task_checkpoint(serde_json::json!({
            "schema": "kars.checkpoint/v1",
            "milestone_id": "build",
            "status": "completed",
            "summary": "Artifact produced",
            "artifacts": "not-an-array"
        }))
        .is_none()
    );
    assert!(
        valid_task_checkpoint(serde_json::json!({
            "schema": "kars.checkpoint/v1",
            "milestone_id": "build",
            "status": "completed",
            "summary": "Artifact produced"
        }))
        .is_some()
    );
}

#[test]
fn clean_objective_strips_loop_scaffold() {
    // A leaked loop scaffold must never reach a title — extract the GOAL.
    let scaffolded = "LOOP: ReAct — Reason + Act\nGOAL: find the Azure/kars star count and write a paragraph\nCYCLE: reason, act, observe\nSUCCESS: a paragraph with the count\nSTOP: when delivered\nSUB-AGENT INHERITANCE: give each sub-agent the same loop";
    assert_eq!(
        clean_objective(scaffolded),
        "find the Azure/kars star count and write a paragraph"
    );
}

#[test]
fn clean_objective_passes_plain_through() {
    assert_eq!(
        clean_objective("Summarize the Q3 report"),
        "Summarize the Q3 report"
    );
}

#[test]
fn clean_objective_strips_bracket_goal() {
    let s = "LOOP: eval-iterate\nGOAL: [[raise CLI test coverage]]\nSTOP: green";
    assert_eq!(clean_objective(s), "raise CLI test coverage");
}

#[test]
fn deliverable_text_strips_fixed_sandbox_banner() {
    let raw = "# ? kars Sandbox - Secure AI Runtime on Azure\n\
        - **Foundry Project:** project\n\
        - **Model:** gpt\n\
        - **Sandbox ID:** run-1\n\
        - **Security:** isolated\n\
        - **Capabilities:** tools and reasoning\n\n\
        [[NO_MATERIAL_CHANGE]] nothing changed.";
    assert_eq!(
        super::deliverable_text(raw),
        "[[NO_MATERIAL_CHANGE]] nothing changed."
    );
}

#[test]
fn deliverable_text_repairs_legacy_question_mark_replacements() {
    assert_eq!(
        super::deliverable_text("1? Role?plan: non?root; GHSA?w8wr?v893?vjvp"),
        "1. Role-plan: non-root; GHSA-w8wr-v893-vjvp"
    );
}

#[test]
fn real_deliverable_gates_error_and_no_change() {
    use super::is_real_deliverable;
    assert!(!is_real_deliverable(Some("error"), "anything"));
    assert!(!is_real_deliverable(Some("ok"), "   "));
    assert!(!is_real_deliverable(
        Some("ok"),
        "[[NO_MATERIAL_CHANGE]] nothing changed"
    ));
    assert!(!is_real_deliverable(
        Some("ok"),
        "kars Sandbox - Secure AI Runtime on Azure\nSandbox ID: run-1\nSecurity: isolated\nCapabilities: tools\n[[NO_MATERIAL_CHANGE]] nothing changed"
    ));
    assert!(is_real_deliverable(Some("ok"), "Here is the report."));
    assert!(is_real_deliverable(None, "Some output"));
    // A budget-blocked ok-run is NOT a deliverable.
    assert!(!is_real_deliverable(
        Some("ok"),
        "API call failed after 3 retries: HTTP 429: Daily token budget exceeded (23131/20000 tokens)."
    ));
    assert!(!is_real_deliverable(
        Some("ok"),
        "unexpected tokens remaining in message header: Some(...)"
    ));
    assert!(!is_real_deliverable(
        Some("ok"),
        "assignment progress lease expired after 90s without renewal"
    ));
    assert!(is_real_deliverable(
        Some("ok"),
        "Completed remediation successfully. A prior child reported assignment progress lease expired, but its replacement delivered."
    ));
}

#[test]
fn failed_output_cannot_create_pull_request_deliverables() {
    let data = std::collections::BTreeMap::from([
        ("status".to_string(), "error".to_string()),
        (
            "output".to_string(),
            "Claimed https://github.com/example/repo/pull/134".to_string(),
        ),
    ]);
    assert!(deliverable_pull_requests(&data).is_empty());
}

#[test]
fn classify_blocked_detects_budget_and_parses_pair() {
    use super::classify_blocked;
    let b = classify_blocked(
        Some("ok"),
        "API call failed after 3 retries: HTTP 429: Daily token budget exceeded (23131/20000 tokens).",
    )
    .expect("budget block detected");
    assert_eq!(b.reason, "budget");
    assert_eq!(b.spent, Some(23131));
    assert_eq!(b.limit, Some(20000));
    // A real deliverable is not blocked.
    assert!(classify_blocked(Some("ok"), "Here is the finished report.").is_none());
    // An error run is handled elsewhere, not as blocked.
    assert!(classify_blocked(Some("error"), "Daily token budget exceeded").is_none());
}

#[test]
fn assignment_dto_uses_the_web_snake_case_contract() {
    let event = TaskAssignmentEventDto {
        sequence: 3,
        event_id: "root:3".into(),
        task_id: "root".into(),
        event_type: "child_progress".into(),
        state: "Completed".into(),
        at: "2026-07-20T12:00:00Z".into(),
        worker_did: Some("did:agt:worker".into()),
        stage: Some("child_handback".into()),
        child_task_id: Some("child-1".into()),
        child_role: Some("reviewer".into()),
        outcome: Some("success".into()),
        message: None,
    };
    let value = serde_json::to_value(event).expect("serialize assignment event");
    assert_eq!(value["child_task_id"], "child-1");
    assert_eq!(value["child_role"], "reviewer");
    assert_eq!(value["event_type"], "child_progress");
    assert!(value.get("childTaskId").is_none());
}

#[test]
fn deliverable_excerpt_strips_noise() {
    use super::deliverable_excerpt;
    let raw = "[[NO_MATERIAL_CHANGE]]\n# Heading\n| a | b |\n---\nThe repo star count is 1,234.";
    let ex = deliverable_excerpt(raw);
    assert!(ex.contains("star count"));
    assert!(!ex.contains("NO_MATERIAL_CHANGE"));
    assert!(!ex.contains('|'));
}

#[test]
fn team_run_names_are_detected() {
    use super::queries::regex_lite_is_team_run;
    assert!(regex_lite_is_team_run("kars-repo-health-run-1783099875"));
    assert!(regex_lite_is_team_run("ci-monitor-team-run-42"));
    // Standalone missions and non-numeric suffixes are NOT team runs.
    assert!(!regex_lite_is_team_run("audit-the-readme"));
    assert!(!regex_lite_is_team_run("some-run-abc"));
    assert!(!regex_lite_is_team_run("foo-run-"));
    assert!(!regex_lite_is_team_run("plainname"));
}

#[test]
fn deliverable_excerpt_drops_markdown_wrapped_sentinel() {
    use super::deliverable_excerpt;
    // Bold-/emphasis-wrapped sentinel must still be recognized and dropped
    // (regression: it used to leak into the excerpt because the sentinel
    // check ran before markdown-wrapping was stripped).
    let raw = "**[[NO_MATERIAL_CHANGE]]** +3 stars\nThe repo now has 1,234 stars.";
    let ex = deliverable_excerpt(raw);
    assert!(
        !ex.contains("NO_MATERIAL_CHANGE"),
        "excerpt leaked sentinel: {ex}"
    );
    assert!(ex.contains("1,234 stars"));
}

#[test]
fn clean_display_name_prefers_intent_over_scaffold() {
    use super::clean_display_name;
    // Scaffold display name -> derive (capitalized) from objective.
    assert_eq!(
        clean_display_name(
            &Some("LOOP: ReAct".to_string()),
            "GOAL: count the stars\nSTOP: done"
        ),
        Some("Count the stars".to_string())
    );
    // Real display name -> kept.
    assert_eq!(
        clean_display_name(&Some("Weekly repo digest".to_string()), "whatever"),
        Some("Weekly repo digest".to_string())
    );
    // A conversational prompt pasted into the display slot is NOT a title —
    // derive a concise one: strip the lead-in, shorten the URL, drop the
    // trailing "- …" condition tail, capitalize.
    assert_eq!(
        clean_display_name(
            &Some(
                "Can you please check https://github.com/Azure/kars and analyse all dependabot PR"
                    .to_string()
            ),
            "Can you please check https://github.com/Azure/kars and analyse all dependabot PRs - categorize the ones which are safe to merge",
        ),
        Some("Check Azure/kars and analyse all dependabot PRs".to_string())
    );
}

#[test]
fn concise_title_strips_lead_in_and_shortens_url() {
    use super::presentation::concise_title;
    assert_eq!(
        concise_title("I need you to summarise https://example.com/reports/q3 today"),
        "Summarise q3 today".to_string()
    );
    // Long objective is capped at a word boundary with an ellipsis.
    let long = "review every open pull request across the entire organisation and produce a ranked risk report";
    let t = concise_title(long);
    assert!(t.chars().count() <= 57, "title too long: {t}");
    assert!(t.ends_with('…'), "expected ellipsis: {t}");
    assert!(t.starts_with("Review "), "expected capitalized start: {t}");
}

#[test]
fn ttl_human_to_iso8601() {
    assert_eq!(normalize_ttl("2h"), "PT2H");
    assert_eq!(normalize_ttl("30m"), "PT30M");
    assert_eq!(normalize_ttl("24h"), "PT24H");
    assert_eq!(normalize_ttl("1d"), "P1D");
    assert_eq!(normalize_ttl("90s"), "PT90S");
    assert_eq!(normalize_ttl(" 8 h "), "PT8H");
}

#[test]
fn ttl_passthrough_and_fallback() {
    assert_eq!(normalize_ttl("PT2H"), "PT2H"); // already ISO
    assert_eq!(normalize_ttl("pt45m"), "PT45M"); // uppercased
    assert_eq!(normalize_ttl(""), "PT2H"); // empty → default
    assert_eq!(normalize_ttl("garbage"), "PT2H"); // unrecognized → default
    assert_eq!(normalize_ttl("0h"), "PT2H"); // zero → default
}
