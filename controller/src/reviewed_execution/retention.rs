// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{kars_team::KarsTeam, team_memory::RunArchive};

// Historical reads never issue a Grant or relax the active execution checks.
async fn completed(
    client: &Client,
    task: &KarsTask,
    phase: &ExecutionPhase,
    required: Vec<String>,
    peers: Option<&Value>,
) -> Result<(Value, ConfigMap)> {
    live(&task.metadata)?;
    let nonce = ann(task, RUN).ok_or_else(|| anyhow::anyhow!("missing retained run nonce"))?;
    ensure!(
        ann(task, COMPLETE) == Some(nonce),
        "retained run is not completed"
    );
    let key = digest(serde_json::to_vec(&json!([task.uid(), nonce]))?)[7..47].to_owned();
    let api = maps(client, task);
    let attempt = api.get(&format!("kars-mission-attempt-{key}")).await?;
    evidence::owned(&attempt, task)?;
    let raw = attempt
        .data
        .as_ref()
        .and_then(|d| d.get("attempt.json"))
        .filter(|s| s.len() <= 900 * 1024)
        .ok_or_else(|| anyhow::anyhow!("retained attempt missing or oversized"))?;
    let record = crate::mission_record::decode(raw)?;
    evidence::success_with_outputs(&record, task, phase, required, peers)?;
    let output = api.get(&format!("kars-mission-output-{key}")).await?;
    let artifacts = api.get(&format!("kars-mission-artifacts-{key}")).await?;
    evidence::canonical(&output, task, &key, "output")?;
    evidence::canonical(&artifacts, task, &key, "artifacts")?;
    evidence::projection(&record, &output, &artifacts)?;
    for prior in [&attempt, &output, &artifacts] {
        ensure!(
            api.get(&prior.name_any()).await? == *prior,
            "retained evidence changed during read"
        );
    }
    current(client, task).await?;
    Ok((record, artifacts))
}

fn document(task: &KarsTask, record: &Value, name: String, artifact_name: &str) -> Result<Input> {
    let content = record["reply"]["artifacts"][artifact_name]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("retained artifact missing"))?
        .to_owned();
    let field = |key: &str| -> Result<String> {
        Ok(record["candidate"][key]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("retained source missing {key}"))?
            .into())
    };
    Ok(Input {
        name,
        source: Source {
            namespace: task.namespace().unwrap(),
            task_name: task.name_any(),
            task_uid: task.uid().unwrap(),
            sandbox_uid: field("sandboxUid")?,
            pod_uid: field("podUid")?,
            run_nonce: ann(task, RUN).unwrap().into(),
            assignment_id: record["assignment"]["assignmentId"]
                .as_str()
                .unwrap()
                .into(),
            agent_did: field("agentDid")?,
            artifact_name: artifact_name.into(),
        },
        sha256: digest(&content),
        content,
    })
}

fn retained_inputs_match(
    actual: &[Input],
    memory: Vec<Input>,
    documents: Vec<Input>,
    handoffs: &[Input],
) -> bool {
    let mut full = memory.clone();
    full.extend(documents);
    full.sort_by(|a, b| a.name.cmp(&b.name));
    // Pre-full-export runs remain readable for their retention lifetime. Their
    // sealed inputs prove summary-only consumption, not review of the full files.
    let mut historical = memory;
    historical.extend_from_slice(handoffs);
    historical.sort_by(|a, b| a.name.cmp(&b.name));
    actual == full || actual == historical
}

#[cfg(test)]
#[test]
fn retained_inputs_accept_exact_full_or_historical_sets_without_partial_exports() {
    let document = |name: &str| -> Input {
        serde_json::from_value(json!({
            "name": name, "content": name, "sha256": digest(name),
            "source": {"namespace":"default", "taskName":"phase", "taskUid":"task",
                "sandboxUid":"sandbox", "podUid":"pod", "runNonce":"nonce",
                "assignmentId":"assignment", "agentDid":format!("did:mesh:{}", "a".repeat(32)),
                "artifactName":name}
        }))
        .unwrap()
    };
    let memory = document("memory-0.md");
    let draft = document("r0-p0--0-draft.md");
    let csv = document("r0-p0--2-plan.csv");
    let handoff = document("r0-p0.md");
    let check = |actual: &[Input]| {
        retained_inputs_match(
            actual,
            vec![memory.clone()],
            vec![draft.clone(), csv.clone(), handoff.clone()],
            std::slice::from_ref(&handoff),
        )
    };
    let full = vec![memory.clone(), draft.clone(), csv.clone(), handoff.clone()];
    let historical = vec![memory.clone(), handoff.clone()];
    assert!(check(&full));
    assert!(check(&historical));
    assert!(!check(&[memory.clone(), draft.clone(), handoff.clone()]));
    assert!(!check(std::slice::from_ref(&handoff)));
    for valid in [full, historical] {
        let mut changed = valid.clone();
        changed[1].source.pod_uid = "replacement".into();
        assert!(!check(&changed));
        let mut changed = valid.clone();
        changed[1].content.push_str(" changed");
        assert!(!check(&changed));
        let mut changed = valid.clone();
        changed[1].sha256 = digest("changed");
        assert!(!check(&changed));
        let mut changed = valid.clone();
        changed.push(document("unknown.md"));
        assert!(!check(&changed));
        let mut changed = valid;
        changed.reverse();
        assert!(!check(&changed));
    }
}

async fn retained_state(
    client: &Client,
    team: &KarsTeam,
    root: &KarsTask,
) -> Result<(ConfigMap, State)> {
    ensure!(
        root.namespace() == team.namespace()
            && root.metadata.owner_references.as_ref()
                == Some(&vec![crate::kars_team_reconciler::team_owner_ref(team)?]),
        "retained run is outside exact Team custody"
    );
    retained_root_state(client, root).await
}

async fn retained_root_state(client: &Client, root: &KarsTask) -> Result<(ConfigMap, State)> {
    let p = plan(root).ok_or_else(|| anyhow::anyhow!("retained execution plan missing"))?;
    crate::task_execution_plan::validate_filesystem_execution(p).map_err(anyhow::Error::msg)?;
    live(&root.metadata)?;
    ensure!(
        ann(root, ROOT).is_none(),
        "retained execution is not a root"
    );
    crate::kars_team_reconciler::requests::validate_work_run(root)?;
    team_roles::validate(root)?;
    let pin: Pin = serde_json::from_str(
        ann(root, PIN).ok_or_else(|| anyhow::anyhow!("retained state pin missing"))?,
    )?;
    let map = maps(client, root).get(&pin.name).await?;
    ensure!(
        map.uid().as_ref() == Some(&pin.uid),
        "retained state replaced"
    );
    owner(&map.metadata, root)?;
    let saved: State = serde_json::from_str(
        map.data
            .as_ref()
            .and_then(|d| d.get("state.json"))
            .ok_or_else(|| anyhow::anyhow!("retained state missing"))?,
    )?;
    let original_generation = saved.admission["taskGeneration"]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or_else(|| anyhow::anyhow!("invalid retained generation"))?;
    ensure!(
        root.metadata
            .generation
            .is_some_and(|n| n >= original_generation),
        "retained generation is ahead of the Task"
    );
    // The sealed request excludes only launch; ordinary retirement increments
    // generation. Every other spec, objective, role and admission binding must match.
    let mut admitted = root.clone();
    admitted.metadata.generation = Some(original_generation);
    let s = state(&map, &admitted)?;
    Ok((map, s))
}

async fn final_phase_documents(
    client: &Client,
    root: &KarsTask,
    map: &ConfigMap,
    s: &State,
) -> Result<(Vec<Input>, Vec<Input>, Vec<KarsTask>)> {
    let p = &s.plan;
    let mut handoffs = Vec::new();
    let mut phase_documents = Vec::new();
    let mut selected = Vec::new();
    for (r, role) in p.roles.iter().enumerate() {
        let parent_reservation = s
            .children
            .get(&slot(r, None))
            .ok_or_else(|| anyhow::anyhow!("retained role was not reserved"))?;
        let parent = tasks(client, root).get(&parent_reservation.name).await?;
        check_child(
            &parent,
            &child(root, root, s, map, r, None)?,
            root,
            Some(parent_reservation),
        )?;
        let index = role.phases.len() - 1;
        let reservation = s
            .children
            .get(&slot(r, Some(index)))
            .ok_or_else(|| anyhow::anyhow!("retained final phase was not reserved"))?;
        let task = tasks(client, root).get(&reservation.name).await?;
        check_child(
            &task,
            &child(root, &parent, s, map, r, Some(index))?,
            &parent,
            Some(reservation),
        )?;
        let (record, _) = completed(
            client,
            &task,
            &role.phases[index],
            vec![HANDOFF.into()],
            peer_context(s, map, &slot(r, Some(index))).as_ref(),
        )
        .await?;
        let handoff = document(&task, &record, format!("r{r}-p{index}.md"), HANDOFF)?;
        phase_documents.extend(aliased_phase_documents(
            &record,
            handoff.source.clone(),
            handoff.name.clone(),
        )?);
        handoffs.push(handoff);
        selected.extend([parent, task]);
    }
    handoffs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((handoffs, phase_documents, selected))
}

fn synthesis_phase(p: &ExecutionPlan) -> ExecutionPhase {
    ExecutionPhase {
        name: "synthesis".into(),
        objective: p.synthesis.objective.clone(),
        capabilities: p.synthesis.capabilities.clone(),
        required_tool_calls: vec![],
        min_tool_calls: 0,
        max_tool_calls: p.synthesis.max_tool_calls,
        fresh_context: true,
        execution_mode: p.synthesis.execution_mode.clone(),
    }
}

async fn recheck(
    client: &Client,
    root: &KarsTask,
    map: &ConfigMap,
    selected: &[KarsTask],
) -> Result<()> {
    for task in selected {
        current(client, task).await?;
    }
    ensure!(
        maps(client, root).get(&map.name_any()).await? == *map,
        "retained state changed"
    );
    current(client, root).await
}

async fn retained_principal(
    client: &Client,
    root: &KarsTask,
    map: &ConfigMap,
    s: &State,
    failed: bool,
) -> Result<(Value, ConfigMap, Vec<Input>, Vec<Input>, Vec<KarsTask>)> {
    let (handoffs, documents, selected) = final_phase_documents(client, root, map, s).await?;
    let phase = synthesis_phase(&s.plan);
    let required = s.plan.deliverables.iter().map(|d| d.name.clone()).collect();
    let peers = peer_context(s, map, "synthesis");
    let (record, evidence) = if failed {
        failed_evidence(client, root, &phase, required, peers.as_ref())
            .await?
            .ok_or_else(|| anyhow::anyhow!("retained principal has no verified failure"))?
    } else {
        completed(client, root, &phase, required, peers.as_ref()).await?
    };
    let actual: Vec<Input> = serde_json::from_value(
        record["candidate"]
            .get("inputArtifacts")
            .cloned()
            .unwrap_or(json!([])),
    )?;
    ensure!(
        retained_inputs_match(
            &actual,
            record_base_inputs(root, &actual, &documents)?,
            documents.clone(),
            &handoffs
        ),
        "principal inputs differ from exact retained memory and peer handoffs"
    );
    Ok((record, evidence, documents, handoffs, selected))
}

pub(super) async fn referenced_documents(
    client: &Client,
    root: &KarsTask,
    references: &[InputReference],
) -> Result<Vec<Input>> {
    let owners = root
        .metadata
        .owner_references
        .as_ref()
        .filter(|owners| owners.len() == 1 && owners[0].kind == "KarsTeam")
        .ok_or_else(|| anyhow::anyhow!("revision references require exact Team custody"))?;
    let binding = |task: &KarsTask| -> Result<crate::kars_team_reconciler::TeamWorkBinding> {
        Ok(serde_json::from_str(
            ann(task, "kars.azure.com/team-work")
                .ok_or_else(|| anyhow::anyhow!("revision source has no work binding"))?,
        )?)
    };
    let current_work = binding(root)?;
    let diagnostic_roles = revision_manifest(root)?
        .ok_or_else(|| anyhow::anyhow!("missing revision manifest"))?
        .diagnostic_roles;
    let mut origins = BTreeMap::new();
    for reference in references {
        if origins.contains_key(&reference.source.task_name) {
            continue;
        }
        let task = tasks(client, root).get(&reference.source.task_name).await?;
        ensure!(
            task.uid().as_deref() == Some(reference.source.task_uid.as_str())
                && ann(&task, RUN) == Some(reference.source.run_nonce.as_str()),
            "revision producer replaced"
        );
        origins.insert(
            reference.source.task_name.clone(),
            ann(&task, ROOT)
                .unwrap_or(&reference.source.task_name)
                .to_owned(),
        );
    }
    let mut retained = BTreeMap::new();
    let mut snapshots = Vec::new();
    for name in origins.values().collect::<std::collections::BTreeSet<_>>() {
        let prior = tasks(client, root).get(name).await?;
        ensure!(
            prior.namespace() == root.namespace()
                && prior.metadata.owner_references.as_ref() == Some(owners),
            "revision producer is outside exact Team custody"
        );
        let prior_work = binding(&prior)?;
        ensure!(
            !current_work.backlog_uid.is_empty()
                && !current_work.item_id.is_empty()
                && current_work.backlog_uid == prior_work.backlog_uid
                && current_work.item_id == prior_work.item_id,
            "revision producer is outside work custody"
        );
        let (map, s) = retained_root_state(client, &prior).await?;
        let failed = !references
            .iter()
            .any(|r| r.source.task_name == prior.name_any());
        let (record, _, documents, _, selected) =
            retained_principal(client, &prior, &map, &s, failed).await?;
        if failed {
            ensure!(
                diagnostic_roles.is_empty() || diagnostic_roles == revision_roles(&s.plan),
                "diagnostic routing differs from retained source plan"
            );
            for document in documents {
                retained.insert(
                    (
                        document.source.task_name.clone(),
                        document.source.artifact_name.clone(),
                    ),
                    document,
                );
            }
        } else {
            for required in &s.plan.deliverables {
                let document = document(&prior, &record, required.name.clone(), &required.name)?;
                retained.insert((prior.name_any(), required.name.clone()), document);
            }
        }
        snapshots.push((prior, map, selected));
    }
    let documents = references
        .iter()
        .map(|reference| {
            let mut document = retained
                .get(&(
                    reference.source.task_name.clone(),
                    reference.source.artifact_name.clone(),
                ))
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!("revision document missing from retained producer")
                })?;
            if reference.name.starts_with("revision-unaccepted-") {
                let mut alias = format!("revision-unaccepted-{}", document.name);
                alias.truncate(128);
                ensure!(
                    reference.name == alias,
                    "diagnostic alias differs from retained phase"
                );
            }
            document.name = reference.name.clone();
            ensure!(
                InputReference::from(&document) == *reference,
                "revision document differs from its seal"
            );
            Ok(document)
        })
        .collect::<Result<Vec<_>>>()?;
    validate_revision_documents(root, &documents)?;
    for (prior, map, selected) in snapshots {
        recheck(client, &prior, &map, &selected).await?;
    }
    Ok(documents)
}

pub(crate) async fn retained_team_run(
    client: &Client,
    team: &KarsTeam,
    root: &KarsTask,
) -> Result<Option<RunArchive>> {
    if plan(root).is_none() {
        return Ok(None);
    }
    let (map, s) = retained_state(client, team, root).await?;
    let (record, _, _, handoffs, selected) =
        retained_principal(client, root, &map, &s, false).await?;
    let documents = record["reply"]["artifacts"]
        .as_object()
        .unwrap()
        .keys()
        .map(|name| document(root, &record, name.clone(), name))
        .collect::<Result<Vec<_>>>()?;
    let output = record["reply"]["output"].as_str().unwrap().to_owned();
    let archive = RunArchive {
        version: 1,
        output_sha256: digest(&output),
        output,
        documents,
        handoffs,
    };
    archive.validate()?;
    recheck(client, root, &map, &selected).await?;
    Ok(Some(archive))
}

/// Failed synthesis exports are never a delivery. Only its independently completed
/// final phases can become explicitly unaccepted inputs for a fresh revision.
pub(crate) async fn retained_failed_team_documents(
    client: &Client,
    team: &KarsTeam,
    root: &KarsTask,
    output: Option<&ConfigMap>,
    failure_digest: &str,
) -> Result<Vec<Input>> {
    let (map, s) = retained_state(client, team, root).await?;
    let Some(output) = output else {
        // A specialist failure may have stopped the graph before synthesis. Verify
        // the exact historical failure, but do not present a partial graph as the
        // final specialists' reviewed export set.
        for (r, role) in s.plan.roles.iter().enumerate() {
            let Some(parent_reservation) = s.children.get(&slot(r, None)) else {
                ensure!(
                    role.phases
                        .iter()
                        .enumerate()
                        .all(|(index, _)| !s.children.contains_key(&slot(r, Some(index)))),
                    "retained phase has no parent reservation"
                );
                continue;
            };
            let parent = tasks(client, root).get(&parent_reservation.name).await?;
            check_child(
                &parent,
                &child(root, root, &s, &map, r, None)?,
                root,
                Some(parent_reservation),
            )?;
            for (index, phase) in role.phases.iter().enumerate() {
                let Some(reservation) = s.children.get(&slot(r, Some(index))) else {
                    continue;
                };
                let task = tasks(client, root).get(&reservation.name).await?;
                check_child(
                    &task,
                    &child(root, &parent, &s, &map, r, Some(index))?,
                    &parent,
                    Some(reservation),
                )?;
                if let Some(record) = failed_phase_evidence(
                    client,
                    &task,
                    phase,
                    peer_context(&s, &map, &slot(r, Some(index))).as_ref(),
                )
                .await?
                {
                    let data = failure_summary(&task, &record)?;
                    if crate::providers::signing::sha256_hex(&serde_json::to_vec(&data)?)
                        == failure_digest
                    {
                        recheck(client, root, &map, &[parent, task]).await?;
                        return Ok(vec![]);
                    }
                }
            }
        }
        bail!("retained reviewed failure evidence changed or disappeared");
    };
    let (_, canonical, mut documents, _, selected) =
        retained_principal(client, root, &map, &s, true).await?;
    ensure!(
        canonical.data == output.data,
        "retained failure projection changed"
    );
    for document in &mut documents {
        document.name = format!("revision-unaccepted-{}", document.name);
        document.name.truncate(128);
    }
    documents.sort_by(|a, b| a.name.cmp(&b.name));
    recheck(client, root, &map, &selected).await?;
    Ok(documents)
}
