// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! The one owned latest-report ConfigMap; publish it before reporting a run consumed.

use super::{ReconcileError, report::Report, workloads::Intent};
use crate::kars_eval::{EvalResult, EvalResultSummary, KarsEval};
use crate::providers::signing::content_digest;
use anyhow::{Context, ensure};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, Client, Resource, ResourceExt, api::PostParams};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug)]
pub(super) struct Receipt {
    pub uid: String,
    pub digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(super) struct Evidence {
    pub eval_uid: String,
    pub eval_generation: i64,
    pub intent: String,
    pub job_name: String,
    pub job_uid: String,
    pub job_generation: i64,
    pub pod_uid: Option<String>,
    pub pod_generation: Option<i64>,
    pub at: String,
    pub created_at: String,
    pub report: Option<Report>,
    pub error: Option<String>,
    pub digest: String,
    pub request_marker: Option<String>,
}

impl Evidence {
    pub fn seal(mut self) -> anyhow::Result<Self> {
        self.digest.clear();
        self.digest = content_digest(&serde_json::to_vec(&self)?);
        Ok(self)
    }

    pub fn valid(&self) -> anyhow::Result<()> {
        ensure!(
            !self.job_uid.is_empty()
                && !self.eval_uid.is_empty()
                && self.job_generation > 0
                && self.eval_generation > 0,
            "invalid retained identity"
        );
        ensure!(
            self.clone().seal()?.digest == self.digest,
            "retained evidence integrity mismatch"
        );
        ensure!(
            self.error.as_deref().is_none_or(|category| matches!(
                category,
                "RunnerTerminatedWithoutReport"
                    | "ReportExitMismatch"
                    | "ReportTimeAttributionMismatch"
                    | "InvalidReportCounts"
                    | "InvalidReportField"
                    | "ReportTooLarge"
                    | "TooManyReports"
                    | "ConflictingReports"
                    | "ReportMissingOrMalformed"
                    | "UnsupportedReportVersion"
                    | "InvalidCorpusDigest"
                    | "ReportAttributionMismatch"
                    | "InvalidReportCases"
                    | "DuplicateReportCase"
                    | "InvalidInconclusiveCase"
                    | "InvalidActualDecision"
                    | "InvalidActualPolicy"
                    | "InvalidExpectedDecision"
                    | "InvalidObservations"
                    | "InvalidPassVerdict"
                    | "InvalidFailVerdict"
                    | "InvalidCaseVerdict"
                    | "InvalidCaseDuration"
                    | "InvalidReportTime"
            )),
            "unrecognized evidence error category"
        );
        if let Some(report) = &self.report {
            ensure!(
                matches!(report.version.as_str(), "v1" | "v2")
                    && report.total as usize <= super::report::MAX_CASES
                    && report.total
                        == report
                            .passed
                            .saturating_add(report.failed)
                            .saturating_add(report.errored)
                    && report.wire["schemaVersion"] == report.version
                    && report.wire["total"] == report.total
                    && report.wire["passed"] == report.passed
                    && report.wire["failed"] == report.failed
                    && report.wire["errored"] == report.errored
                    && report.wire["results"]
                        .as_array()
                        .is_some_and(|cases| cases.len() == report.total as usize),
                "retained report/count disagreement"
            );
        }
        chrono::DateTime::parse_from_rfc3339(&self.at)
            .context("invalid retained completion time")?;
        chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .context("invalid retained creation time")?;
        Ok(())
    }

    pub fn current(&self, intent: &Intent) -> bool {
        self.eval_uid == intent.eval_uid
            && self.eval_generation == intent.generation
            && self.intent == intent.digest
            && self.request_marker == intent.request_marker
    }

    pub fn state(&self) -> &'static str {
        match &self.report {
            Some(report) if !report.qualified() => "RunnerUpgradeRequired",
            Some(report) if self.error.is_none() && report.ready() => "AllPassed",
            Some(report) if self.error.is_none() && report.errored == 0 && report.failed > 0 => {
                "DriftDetected"
            }
            _ => "Inconclusive",
        }
    }

    pub fn drift(&self) -> bool {
        self.error.is_none()
            && self
                .report
                .as_ref()
                .is_some_and(|r| r.qualified() && r.failed > 0)
    }

    pub fn result(&self, label: &str) -> EvalResult {
        let report = self.report.as_ref();
        EvalResult {
            schema_version: report.map_or("v2", |r| r.version.as_str()).into(),
            corpus_digest: report.map_or("", |r| r.corpus_digest.as_str()).into(),
            total: report.map_or(0, |r| r.total),
            passed: report.map_or(0, |r| r.passed),
            failed: report.map_or(0, |r| r.failed),
            errored: report.map_or(0, |r| r.errored),
            corpus_label: match report {
                Some(report) if !report.qualified() => report.wire["corpusName"]
                    .as_str()
                    .unwrap_or("unverified")
                    .into(),
                Some(_) => label.into(),
                None => "unverified".into(),
            },
            job_name: self.job_name.clone(),
            first_failing_cases: report
                .and_then(|r| r.wire["results"].as_array())
                .into_iter()
                .flatten()
                .filter(|case| case["verdict"]["result"] == "Fail")
                .filter_map(|case| case["caseId"].as_str().map(str::to_owned))
                .take(5)
                .collect(),
        }
    }

    pub fn summary(&self) -> EvalResultSummary {
        let result = self.result("");
        EvalResultSummary {
            at: self.at.clone(),
            corpus_digest: result.corpus_digest,
            total: result.total,
            passed: result.passed,
            failed: result.failed,
            errored: result.errored,
            job_name: self.job_name.clone(),
        }
    }
}

pub(super) fn name(eval: &KarsEval) -> String {
    format!("karseval-{}-report", eval.name_any())
}

fn owned(cm: &ConfigMap, eval: &KarsEval) -> anyhow::Result<()> {
    ensure!(
        cm.namespace() == eval.namespace()
            && cm.name_any() == name(eval)
            && cm.metadata.deletion_timestamp.is_none(),
        "latest report location changed"
    );
    super::workloads::require_owner(&cm.metadata, eval)?;
    ensure!(
        cm.binary_data.as_ref().is_none_or(|data| data.is_empty())
            && cm.data.as_ref().is_some_and(|data| data
                .keys()
                .all(|key| matches!(key.as_str(), "report.json" | "evidence.json"))),
        "latest report contains unrelated data; preserved"
    );
    ensure!(
        cm.metadata.uid.as_deref().is_some_and(|s| !s.is_empty())
            && cm
                .metadata
                .resource_version
                .as_deref()
                .is_some_and(|s| !s.is_empty()),
        "latest report UID/resourceVersion missing"
    );
    Ok(())
}

pub(super) async fn read(
    client: &Client,
    eval: &KarsEval,
) -> anyhow::Result<(Option<Evidence>, bool, Option<Receipt>)> {
    let api = Api::<ConfigMap>::namespaced(client.clone(), super::workloads::namespace(eval)?);
    let Some(cm) = api
        .get_opt(&name(eval))
        .await
        .context("read latest report")?
    else {
        return Ok((None, false, None));
    };
    owned(&cm, eval)?;
    let data = cm.data.as_ref().context("latest report has no data")?;
    let Some(value) = data.get("evidence.json") else {
        // Older, exclusively owned per-case reports remain readable in place.
        ensure!(
            data.get("report.json")
                .is_some_and(|s| s.len() <= super::report::MAX_REPORT_BYTES),
            "invalid legacy latest report"
        );
        let legacy: serde_json::Value =
            serde_json::from_str(&data["report.json"]).context("malformed legacy report")?;
        ensure!(
            legacy["schemaVersion"] == "v1",
            "unbound latest report is not legacy v1"
        );
        return Ok((None, true, None));
    };
    ensure!(
        value.len() <= super::report::MAX_REPORT_BYTES,
        "retained evidence exceeds limit"
    );
    let evidence: Evidence =
        serde_json::from_str(value).context("decode latest report evidence")?;
    evidence.valid()?;
    let report: serde_json::Value =
        serde_json::from_str(data.get("report.json").context("report missing")?)?;
    ensure!(
        report == wire(&evidence),
        "latest report/evidence disagreement"
    );
    let receipt = Receipt {
        uid: cm.metadata.uid.clone().context("report UID missing")?,
        digest: evidence.digest.clone(),
    };
    Ok((Some(evidence), true, Some(receipt)))
}

fn wire(evidence: &Evidence) -> serde_json::Value {
    let mut wire = evidence
        .report
        .as_ref()
        .map(|r| r.wire.clone())
        .unwrap_or_else(|| {
            json!({
                "schemaVersion":"v2", "outcome":"Inconclusive", "category":evidence.error,
                "total":0, "passed":0, "failed":0, "errored":0, "results":[],
            })
        });
    let state = evidence.state();
    wire["outcome"] = json!(match state {
        "AllPassed" => "Passed",
        "DriftDetected" => "PolicyFailure",
        _ => "Inconclusive",
    });
    wire["qualification"] = json!(state);
    if let Some(category) = &evidence.error {
        wire["errorCategory"] = json!(category);
    }
    wire
}

pub(super) async fn publish(
    client: &Client,
    eval: &KarsEval,
    intent: &Intent,
    evidence: &Evidence,
) -> Result<Receipt, ReconcileError> {
    let result = async {
        evidence.valid()?;
        let report = serde_json::to_string(&wire(evidence))?;
        let encoded = serde_json::to_string(evidence)?;
        ensure!(
            report.len() + encoded.len() <= super::report::MAX_REPORT_BYTES,
            "latest report exceeds bounded store"
        );
        let api = Api::<ConfigMap>::namespaced(client.clone(), super::workloads::namespace(eval)?);
        let existing = api
            .get_opt(&name(eval))
            .await
            .context("read report before persistence")?;
        if let Some(old) = &existing {
            owned(old, eval)?;
            if let Some(encoded_old) = old.data.as_ref().and_then(|data| data.get("evidence.json"))
            {
                let previous: Evidence =
                    serde_json::from_str(encoded_old).context("decode previous immutable run")?;
                previous.valid()?;
                ensure!(
                    previous.job_uid != evidence.job_uid
                        || previous.report.is_none()
                        || previous.digest == evidence.digest,
                    "terminal run evidence changed; prior report preserved"
                );
            }
            if old.data.as_ref().and_then(|data| data.get("evidence.json")) == Some(&encoded) {
                ensure!(
                    old.data.as_ref().and_then(|data| data.get("report.json")) == Some(&report),
                    "idempotent evidence has different report"
                );
                intent.revalidate(client, eval).await?;
                return Ok(Receipt {
                    uid: old.metadata.uid.clone().context("report UID missing")?,
                    digest: evidence.digest.clone(),
                });
            }
        }
        intent.revalidate(client, eval).await?;
        let mut cm = existing.clone().unwrap_or_else(|| ConfigMap {
            metadata: kube::api::ObjectMeta {
                name: Some(name(eval)),
                namespace: eval.namespace(),
                owner_references: Some(vec![
                    eval.controller_owner_ref(&())
                        .context("Eval owner UID missing")?,
                ]),
                labels: Some(
                    [
                        (
                            "app.kubernetes.io/managed-by".into(),
                            "kars-controller".into(),
                        ),
                        (super::LABEL_KEY_CLAW_EVAL.into(), eval.name_any()),
                    ]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        });
        cm.data = Some(
            [
                ("report.json".into(), report),
                ("evidence.json".into(), encoded),
            ]
            .into(),
        );
        let written = if existing.is_some() {
            api.replace(&name(eval), &PostParams::default(), &cm)
                .await
                .context("persist latest report with UID/RV")?
        } else {
            api.create(&PostParams::default(), &cm)
                .await
                .context("create latest report without adoption")?
        };
        owned(&written, eval)?;
        ensure!(
            written.data == cm.data,
            "report persistence response differs from verified evidence"
        );
        Ok::<_, anyhow::Error>(Receipt {
            uid: written
                .metadata
                .uid
                .context("persisted report UID missing")?,
            digest: evidence.digest.clone(),
        })
    }
    .await;
    result.map_err(ReconcileError::Evidence)
}
