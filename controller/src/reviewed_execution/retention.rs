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
    let record: Value = serde_json::from_str(raw)?;
    evidence::success_with_outputs(&record, task, phase, required)?;
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

pub(crate) async fn retained_team_run(
    client: &Client,
    team: &KarsTeam,
    root: &KarsTask,
) -> Result<Option<RunArchive>> {
    let Some(p) = plan(root) else { return Ok(None) };
    crate::task_execution_plan::validate_filesystem_execution(p).map_err(anyhow::Error::msg)?;
    live(&root.metadata)?;
    ensure!(
        ann(root, ROOT).is_none()
            && root.namespace() == team.namespace()
            && root.metadata.owner_references.as_ref()
                == Some(&vec![crate::kars_team_reconciler::team_owner_ref(team)?]),
        "retained run is outside exact Team custody"
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
    let mut handoffs = Vec::new();
    let mut selected = Vec::new();
    for (r, role) in p.roles.iter().enumerate() {
        let parent_reservation = s
            .children
            .get(&slot(r, None))
            .ok_or_else(|| anyhow::anyhow!("retained role was not reserved"))?;
        let parent = tasks(client, root).get(&parent_reservation.name).await?;
        check_child(
            &parent,
            &child(root, root, &s, &map, r, None)?,
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
            &child(root, &parent, &s, &map, r, Some(index))?,
            &parent,
            Some(reservation),
        )?;
        let (record, _) =
            completed(client, &task, &role.phases[index], vec![HANDOFF.into()]).await?;
        handoffs.push(document(
            &task,
            &record,
            format!("r{r}-p{index}.md"),
            HANDOFF,
        )?);
        selected.extend([parent, task]);
    }
    handoffs.sort_by(|a, b| a.name.cmp(&b.name));
    let phase = ExecutionPhase {
        name: "synthesis".into(),
        objective: p.synthesis.objective.clone(),
        capabilities: p.synthesis.capabilities.clone(),
        required_tool_calls: vec![],
        min_tool_calls: 0,
        max_tool_calls: p.synthesis.max_tool_calls,
        fresh_context: true,
    };
    let (record, _) = completed(
        client,
        root,
        &phase,
        p.deliverables.iter().map(|d| d.name.clone()).collect(),
    )
    .await?;
    let actual: Vec<Input> = serde_json::from_value(record["candidate"]["inputArtifacts"].clone())?;
    let mut expected = memory_inputs(root)?;
    expected.extend(handoffs.clone());
    expected.sort_by(|a, b| a.name.cmp(&b.name));
    ensure!(
        actual == expected,
        "principal inputs differ from exact retained memory and peer handoffs"
    );
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
    for task in &selected {
        current(client, task).await?;
    }
    ensure!(
        maps(client, root).get(&map.name_any()).await? == map,
        "retained state changed"
    );
    current(client, root).await?;
    Ok(Some(archive))
}
