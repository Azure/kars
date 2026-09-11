// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Read only terminal, currently attributed native Job/Pod evidence.

use super::{
    ResolvedCorpus,
    evidence::{self, Evidence},
    report,
    workloads::{self, Intent},
};
use crate::kars_eval::{EvalResult, EvalResultSummary, KarsEval, push_history_bounded};
use anyhow::{Context, ensure};
use k8s_openapi::api::{
    batch::v1::{CronJob, Job},
    core::v1::Pod,
};
use kube::{
    Api, Client, ResourceExt,
    api::{ListParams, LogParams},
};

pub(super) struct Observation {
    pub history: Vec<EvalResultSummary>,
    pub result: Option<EvalResult>,
    pub last_run_at: Option<String>,
    pub state: &'static str,
    pub drift: bool,
    pub has_report: bool,
    pub receipt: Option<evidence::Receipt>,
}

struct Reader<'a> {
    client: &'a Client,
    jobs: &'a Api<Job>,
    cronjobs: &'a Api<CronJob>,
    pods: &'a Api<Pod>,
    eval: &'a KarsEval,
    intent: &'a Intent,
    corpus: &'a ResolvedCorpus,
}

pub(super) async fn observe(
    client: &Client,
    eval: &KarsEval,
    intent: &Intent,
    corpus: &ResolvedCorpus,
) -> anyhow::Result<Observation> {
    let ns = workloads::namespace(eval)?;
    let prior = eval.status.clone().unwrap_or_default();
    let (retained, had_report, stored_receipt) = evidence::read(client, eval).await?;
    let legacy_store = had_report && retained.is_none();
    let cache_authorized = stored_receipt.as_ref().is_some_and(|receipt| {
        prior.report_config_map_uid.as_ref() == Some(&receipt.uid)
            && prior.report_evidence_digest.as_ref() == Some(&receipt.digest)
    });
    let unverified_cache = had_report && !legacy_store && !cache_authorized;
    let jobs = Api::<Job>::namespaced(client.clone(), ns);
    let cronjobs = Api::<CronJob>::namespaced(client.clone(), ns);
    let pods = Api::<Pod>::namespaced(client.clone(), ns);
    let fulfilled = retained
        .as_ref()
        .filter(|record| {
            cache_authorized && record.fulfills_request(intent) && !record.current(intent)
        })
        .cloned();
    if let Some(record) = &fulfilled {
        verify_fulfillment_source(&jobs, eval, record).await?;
    }
    let list = jobs
        .list(&ListParams::default().labels(&format!(
            "{}={}",
            super::LABEL_KEY_CLAW_EVAL,
            eval.name_any()
        )))
        .await
        .context("list evaluator Jobs")?;
    let mut candidates = Vec::new();
    for job in list {
        verify_retained_source(&job, retained.as_ref(), cache_authorized, intent)?;
        let Some(template) = job.spec.as_ref().map(|spec| &spec.template) else {
            continue;
        };
        if !template
            .metadata
            .as_ref()
            .is_some_and(|meta| intent.matches(meta))
        {
            continue;
        }
        verify_job(&job, &cronjobs, eval, intent).await?;
        let created = job
            .metadata
            .creation_timestamp
            .as_ref()
            .context("Job creation timestamp missing")?
            .0
            .to_string();
        candidates.push((created, job));
    }
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.metadata.uid.cmp(&b.1.metadata.uid))
    });
    if let Some(requested) = &intent.request_job
        && !candidates
            .iter()
            .any(|(_, job)| job.name_any() == *requested)
        && let Some(job) = jobs
            .get_opt(requested)
            .await
            .context("read last explicitly requested Job")?
    {
        verify_retained_source(&job, retained.as_ref(), cache_authorized, intent)?;
        if job
            .spec
            .as_ref()
            .and_then(|spec| spec.template.metadata.as_ref())
            .is_some_and(|meta| intent.matches(meta))
        {
            verify_job(&job, &cronjobs, eval, intent).await?;
            let created = job
                .metadata
                .creation_timestamp
                .as_ref()
                .context("Job creation timestamp missing")?
                .0
                .to_string();
            candidates.push((created, job));
            candidates.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then_with(|| a.1.metadata.uid.cmp(&b.1.metadata.uid))
            });
        }
    }
    let latest = candidates.last();
    let explicit_pending = eval
        .annotations()
        .get(crate::kars_eval::ANNOTATION_RUN_NOW)
        .map(String::as_str)
        == Some("true");
    let retained = retained.filter(|evidence| cache_authorized && evidence.current(intent));
    // Historical fulfillment releases only the request gate. The old report remains
    // excluded by current(intent), even when a new scheduled run can now be observed.
    let missing_request = intent.request_job.as_ref().is_some_and(|requested| {
        !candidates
            .iter()
            .any(|(_, job)| job.name_any() == *requested)
    }) && retained.is_none()
        && fulfilled.is_none();
    let requested_running = if let Some((_, job)) = candidates
        .iter()
        .find(|(_, job)| intent.request_job.as_deref() == Some(job.name_any().as_str()))
    {
        workloads::terminal(job)?.is_none()
    } else {
        false
    };
    let selected = if explicit_pending || missing_request || requested_running {
        None
    } else if let Some((_, job)) = latest {
        if let Some((complete, at)) = workloads::terminal(job)? {
            Some(
                read_evidence(
                    &Reader {
                        client,
                        jobs: &jobs,
                        cronjobs: &cronjobs,
                        pods: &pods,
                        eval,
                        intent,
                        corpus,
                    },
                    job,
                    complete,
                    &at,
                )
                .await?,
            )
        } else {
            None
        }
    } else {
        None
    };
    let pending = explicit_pending
        || missing_request
        || requested_running
        || (latest.is_some() && selected.is_none());
    let selected = match (selected, retained) {
        (Some(new), Some(old))
            if new.job_uid == old.job_uid
                && new.job_name == old.job_name
                && new.job_generation == old.job_generation
                && new.at == old.at
                && new.created_at == old.created_at
                && new.report.is_none()
                && new.error.as_deref() == Some("RunnerTerminatedWithoutReport") =>
        {
            Some(old)
        }
        (Some(new), Some(old)) if old.created_at > new.created_at => Some(old),
        (Some(new), _) => Some(new),
        (None, old) => old,
    };
    let mut history = prior.history;
    let mut result = prior.last_result;
    let mut last_run_at = prior.last_run_at;
    let mut state = if legacy_store
        || result
            .as_ref()
            .is_some_and(|result| result.schema_version == "v1")
    {
        "RunnerUpgradeRequired"
    } else if result.is_some() || unverified_cache {
        "Inconclusive"
    } else {
        "Pending"
    };
    let mut drift = false;
    let mut receipt = if cache_authorized {
        stored_receipt.clone()
    } else {
        None
    };
    let has_report = legacy_store || cache_authorized || selected.is_some();
    if let Some(selected) = selected {
        ensure!(selected.current(intent), "selected evidence is not current");
        if let Some(record) = &fulfilled {
            verify_fulfillment_source(&jobs, eval, record).await?;
        }
        receipt = Some(evidence::publish(client, eval, intent, &selected).await?);
        // Durable evidence is the retry key. Status may have lost its previous acknowledgement.
        history.retain(|item| item.job_name != selected.job_name);
        history = push_history_bounded(history, selected.summary());
        result = Some(selected.result(&corpus.label));
        last_run_at = Some(selected.at.clone());
        state = selected.state();
        drift = selected.drift();
    }
    if pending {
        state = "Pending";
        drift = false;
    }
    if intent.target_uid.is_none() && result.is_some() {
        state = "Inconclusive";
        drift = false;
    }
    intent.revalidate(client, eval).await?;
    Ok(Observation {
        history,
        result,
        last_run_at,
        state,
        drift,
        has_report,
        receipt,
    })
}

async fn verify_fulfillment_source(
    jobs: &Api<Job>,
    eval: &KarsEval,
    record: &Evidence,
) -> anyhow::Result<()> {
    let Some(job) = jobs
        .get_opt(&record.job_name)
        .await
        .context("recheck historical request fulfillment")?
    else {
        // Only a receipt authenticated by protected status reaches this path.
        return Ok(());
    };
    let (uid, _) = workloads::identity(&job.metadata)?;
    ensure!(
        uid == record.job_uid
            && job.metadata.generation == Some(record.job_generation)
            && job
                .metadata
                .creation_timestamp
                .as_ref()
                .map(|at| at.0.to_string())
                == Some(record.created_at.clone())
            && workloads::terminal(&job)?.is_some_and(|(_, at)| at == record.at),
        "historical fulfillment Job identity/terminal state changed"
    );
    let stamps = job
        .spec
        .as_ref()
        .and_then(|spec| spec.template.metadata.as_ref())
        .and_then(|meta| meta.annotations.as_ref())
        .context("historical fulfillment producer stamps missing")?;
    ensure!(
        stamps.get(workloads::INTENT) == Some(&record.intent)
            && (record.request_job.is_none()
                || (stamps.get(workloads::LAST_TOKEN) == record.request_marker.as_ref()
                    && stamps.get(workloads::LAST_RUN) == record.request_job.as_ref())),
        "historical fulfillment producer/request changed"
    );
    if eval.annotations().get(workloads::LAST_RUN) == Some(&record.job_name) {
        ensure!(
            workloads::owned_by(
                &job.metadata,
                workloads::namespace(eval)?,
                &record.eval_uid,
                &eval.name_any(),
                "KarsEval",
                "kars.azure.com/v1alpha1",
            ) && job.annotations().get(workloads::RUN_TOKEN) == record.request_marker.as_ref(),
            "historical explicit request token/source changed"
        );
    } else {
        // The protected receipt authenticated this native Job UID and its original
        // owner chain. That historical CronJob may since have been deleted/recreated.
        // Fresh scheduled reports still require the live CronJob UID in verify_job.
        ensure!(
            job.namespace() == eval.namespace()
                && job
                    .metadata
                    .owner_references
                    .as_ref()
                    .is_some_and(|owners| {
                        owners.len() == 1
                            && owners[0].kind == "CronJob"
                            && owners[0].api_version == "batch/v1"
                            && owners[0].name == super::cron_job_name(&eval.name_any())
                            && owners[0].controller == Some(true)
                            && !owners[0].uid.is_empty()
                    })
                && job.annotations().get(workloads::LAST_TOKEN) == record.request_marker.as_ref()
                && job.annotations().get(workloads::LAST_RUN) == record.request_job.as_ref(),
            "historical scheduled request source changed"
        );
    }
    Ok(())
}

fn verify_retained_source(
    job: &Job,
    retained: Option<&Evidence>,
    authorized: bool,
    intent: &Intent,
) -> anyhow::Result<()> {
    if let Some(old) = retained.filter(|old| old.job_name == job.name_any()) {
        ensure!(
            job.metadata.uid.as_deref() == Some(old.job_uid.as_str()),
            "previously observed Job name was replaced"
        );
        if authorized && old.current(intent) {
            ensure!(
                job.spec
                    .as_ref()
                    .and_then(|spec| spec.template.metadata.as_ref())
                    .is_some_and(|meta| intent.matches(meta)),
                "retained Job request/spec intent changed"
            );
        }
    }
    Ok(())
}

async fn verify_job(
    job: &Job,
    cronjobs: &Api<CronJob>,
    eval: &KarsEval,
    intent: &Intent,
) -> anyhow::Result<()> {
    let ns = workloads::namespace(eval)?;
    workloads::identity(&job.metadata)?;
    ensure!(
        job.metadata.generation.is_some_and(|g| g > 0) && job.metadata.deletion_timestamp.is_none(),
        "Job generation/termination changed"
    );
    let template_spec = job
        .spec
        .as_ref()
        .and_then(|spec| spec.template.spec.as_ref())
        .context("Job Pod template missing")?;
    ensure!(
        workloads::contains(&serde_json::to_value(template_spec)?, &intent.pod_spec),
        "Job Pod template differs from the actual current producer"
    );
    if workloads::require_owner(&job.metadata, eval).is_ok() {
        ensure!(
            job.annotations().get(workloads::RUN_TOKEN) == intent.request_marker.as_ref()
                && intent
                    .request_job
                    .as_ref()
                    .is_none_or(|name| *name == job.name_any()),
            "one-shot Job does not match the current request"
        );
        return Ok(());
    }
    ensure!(
        eval.spec.schedule.is_some(),
        "scheduled Job without a current schedule"
    );
    let name = super::cron_job_name(&eval.name_any());
    let cron = cronjobs
        .get(&name)
        .await
        .context("read scheduled Job parent")?;
    workloads::require_owner(&cron.metadata, eval)?;
    let (uid, _) = workloads::identity(&cron.metadata)?;
    ensure!(
        workloads::owned_by(&job.metadata, ns, uid, &name, "CronJob", "batch/v1"),
        "Job is not owned by current evaluator CronJob UID"
    );
    let meta = cron
        .spec
        .as_ref()
        .and_then(|spec| spec.job_template.spec.as_ref())
        .and_then(|spec| spec.template.metadata.as_ref())
        .context("CronJob template missing")?;
    ensure!(intent.matches(meta), "CronJob intent changed");
    let job_meta = cron
        .spec
        .as_ref()
        .and_then(|spec| spec.job_template.metadata.as_ref())
        .context("CronJob Job metadata missing")?;
    ensure!(
        intent.matches(job_meta)
            && job.annotations().get(workloads::LAST_TOKEN) == intent.request_marker.as_ref()
            && job.annotations().get(workloads::LAST_RUN) == intent.request_job.as_ref(),
        "scheduled Job request stamp differs from its current producer"
    );
    let spec = cron
        .spec
        .as_ref()
        .and_then(|spec| spec.job_template.spec.as_ref())
        .and_then(|spec| spec.template.spec.as_ref())
        .context("CronJob Pod template missing")?;
    ensure!(
        workloads::contains(&serde_json::to_value(spec)?, &intent.pod_spec),
        "CronJob Pod template differs from the current producer"
    );
    Ok(())
}

async fn read_evidence(
    reader: &Reader<'_>,
    job: &Job,
    complete: bool,
    at: &str,
) -> anyhow::Result<Evidence> {
    let Reader {
        client,
        jobs,
        cronjobs,
        pods,
        eval,
        intent,
        corpus,
    } = reader;
    let (job_uid, _) = workloads::identity(&job.metadata)?;
    let stamps = job
        .spec
        .as_ref()
        .and_then(|spec| spec.template.metadata.as_ref())
        .and_then(|meta| meta.annotations.as_ref())
        .context("Job request stamps missing")?;
    let mut evidence = Evidence {
        eval_uid: intent.eval_uid.clone(),
        eval_generation: intent.generation,
        intent: intent.digest.clone(),
        job_name: job.name_any(),
        job_uid: job_uid.into(),
        job_generation: job.metadata.generation.context("Job generation missing")?,
        pod_uid: None,
        pod_generation: None,
        at: at.into(),
        created_at: job
            .metadata
            .creation_timestamp
            .as_ref()
            .context("Job creation missing")?
            .0
            .to_string(),
        report: None,
        error: Some("RunnerTerminatedWithoutReport".into()),
        digest: String::new(),
        request_marker: stamps.get(workloads::LAST_TOKEN).cloned(),
        request_job: stamps.get(workloads::LAST_RUN).cloned(),
    };
    let pod_list = pods
        .list(&ListParams::default().labels(&format!("job-name={}", job.name_any())))
        .await
        .context("list Job Pods")?;
    let mut terminal_pods = Vec::new();
    for pod in pod_list {
        if !workloads::owned_by(
            &pod.metadata,
            workloads::namespace(eval)?,
            job_uid,
            &job.name_any(),
            "Job",
            "batch/v1",
        ) {
            continue;
        }
        let phase = pod.status.as_ref().and_then(|s| s.phase.as_deref());
        if !matches!(
            phase,
            Some(
                crate::status::phase::POD_PHASE_SUCCEEDED | crate::status::phase::POD_PHASE_FAILED
            )
        ) {
            continue;
        }
        let terminated = pod
            .status
            .as_ref()
            .and_then(|status| status.container_statuses.as_ref())
            .into_iter()
            .flatten()
            .find(|container| container.name == "runner")
            .and_then(|container| container.state.as_ref())
            .and_then(|state| state.terminated.as_ref());
        let Some(terminated) = terminated else {
            continue;
        };
        let finished = terminated
            .finished_at
            .as_ref()
            .context("terminal runner time missing")?
            .0
            .to_string();
        terminal_pods.push((finished, terminated.exit_code, pod));
    }
    terminal_pods.sort_by(|a, b| a.0.cmp(&b.0));
    if let Some((finished, exit_code, pod)) = terminal_pods.last() {
        let (uid, _) = workloads::identity(&pod.metadata)?;
        ensure!(
            pod.metadata.deletion_timestamp.is_none() && intent.matches(&pod.metadata),
            "Pod intent changed"
        );
        let expected = serde_json::to_value(
            job.spec
                .as_ref()
                .context("Job spec missing")?
                .template
                .spec
                .as_ref()
                .context("Job Pod spec missing")?,
        )?;
        ensure!(
            workloads::contains(&serde_json::to_value(&pod.spec)?, &expected),
            "Pod differs from produced Job spec"
        );
        let before = pods
            .get(&pod.name_any())
            .await
            .context("re-read report Pod")?;
        ensure!(
            before.metadata.uid == pod.metadata.uid
                && before.metadata.generation == pod.metadata.generation
                && before.metadata.owner_references == pod.metadata.owner_references
                && before.metadata.deletion_timestamp.is_none()
                && intent.matches(&before.metadata)
                && serde_json::to_value(&before.status)? == serde_json::to_value(&pod.status)?
                && serde_json::to_value(&before.spec)? == serde_json::to_value(&pod.spec)?,
            "Pod replaced before log read"
        );
        let logs = pods
            .logs(
                &pod.name_any(),
                &LogParams {
                    container: Some("runner".into()),
                    limit_bytes: Some((report::MAX_REPORT_BYTES + 1) as i64),
                    ..Default::default()
                },
            )
            .await
            .context("read owned runner report")?;
        let after = pods
            .get(&pod.name_any())
            .await
            .context("recheck report Pod")?;
        ensure!(
            after.metadata.uid == pod.metadata.uid
                && after.metadata.generation == pod.metadata.generation
                && after.metadata.owner_references == pod.metadata.owner_references
                && after.metadata.deletion_timestamp.is_none()
                && intent.matches(&after.metadata)
                && serde_json::to_value(&after.status)? == serde_json::to_value(&pod.status)?
                && serde_json::to_value(&after.spec)? == serde_json::to_value(&pod.spec)?,
            "Pod changed during log read"
        );
        evidence.pod_uid = Some(uid.into());
        evidence.pod_generation = pod.metadata.generation;
        let parsed_corpus =
            kars_eval_corpus::parse(&corpus.bytes).context("parse current corpus")?;
        match report::parse(&logs, &parsed_corpus, &corpus.digest, &intent.router) {
            Ok(report) => {
                let started = chrono::DateTime::parse_from_rfc3339(
                    report.wire["startedAt"]
                        .as_str()
                        .context("report time missing")?,
                )?;
                let completed = chrono::DateTime::parse_from_rfc3339(
                    report.wire["completedAt"]
                        .as_str()
                        .context("report time missing")?,
                )?;
                let job_created = chrono::DateTime::parse_from_rfc3339(&evidence.created_at)?;
                let pod_finished = chrono::DateTime::parse_from_rfc3339(finished)?;
                // metav1.Time is serialized at whole-second precision. Only the finish
                // second is uncertain: the following second is already out of bounds.
                evidence.error = if started < job_created
                    || completed.timestamp() > pod_finished.timestamp()
                {
                    Some("ReportTimeAttributionMismatch".into())
                } else if (report.exit_code() == *exit_code) && (complete == (*exit_code == 0)) {
                    None
                } else {
                    Some("ReportExitMismatch".into())
                };
                evidence.report = Some(report);
            }
            Err(reason) => evidence.error = Some(reason.into()),
        }
    }
    let current = jobs
        .get(&job.name_any())
        .await
        .context("recheck terminal Job")?;
    verify_job(&current, cronjobs, eval, intent).await?;
    ensure!(
        current.metadata.uid == job.metadata.uid
            && current.metadata.generation == job.metadata.generation
            && serde_json::to_value(&current.spec)? == serde_json::to_value(&job.spec)?
            && workloads::terminal(&current)? == Some((complete, at.into())),
        "Job changed during report read"
    );
    intent.revalidate(client, eval).await?;
    evidence.seal()
}
