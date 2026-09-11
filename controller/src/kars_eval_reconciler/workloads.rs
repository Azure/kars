// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Current evaluator intent and native workload/Pod provenance.

use crate::{crd::KarsSandbox, kars_eval::KarsEval, providers::signing::content_digest};
use anyhow::{Context, ensure};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams},
};
use serde_json::{Value, json};

pub(super) const INTENT: &str = "kars.azure.com/karseval-intent";
pub(super) const CORPUS: &str = "kars.azure.com/karseval-corpus-digest";
pub(super) const RUN_TOKEN: &str = "kars.azure.com/karseval-run-token";
pub(super) const LAST_RUN: &str = "kars.azure.com/karseval-last-request";
pub(super) const LAST_TOKEN: &str = "kars.azure.com/karseval-last-request-token";

pub(super) fn namespace(eval: &KarsEval) -> anyhow::Result<&str> {
    eval.metadata
        .namespace
        .as_deref()
        .filter(|s| !s.is_empty())
        .context("Eval namespace missing")
}

pub(super) fn identity(meta: &ObjectMeta) -> anyhow::Result<(&str, &str)> {
    match (meta.uid.as_deref(), meta.resource_version.as_deref()) {
        (Some(uid), Some(rv)) if !uid.is_empty() && !rv.is_empty() => Ok((uid, rv)),
        _ => anyhow::bail!("resource UID/resourceVersion missing"),
    }
}

pub(super) fn owned_by(
    meta: &ObjectMeta,
    namespace: &str,
    uid: &str,
    name: &str,
    kind: &str,
    version: &str,
) -> bool {
    meta.namespace.as_deref() == Some(namespace)
        && meta.owner_references.as_ref().is_some_and(|owners| {
            owners.len() == 1
                && owners[0].uid == uid
                && owners[0].name == name
                && owners[0].kind == kind
                && owners[0].api_version == version
                && owners[0].controller == Some(true)
        })
}

pub(super) fn require_owner(meta: &ObjectMeta, eval: &KarsEval) -> anyhow::Result<()> {
    let (uid, _) = identity(&eval.metadata)?;
    ensure!(
        owned_by(
            meta,
            namespace(eval)?,
            uid,
            &eval.name_any(),
            "KarsEval",
            "kars.azure.com/v1alpha1"
        ),
        "resource is not exclusively owned by the current Eval UID"
    );
    ensure!(
        meta.deletion_timestamp.is_none(),
        "owned resource is terminating"
    );
    Ok(())
}

pub(super) fn same_eval(current: &KarsEval, expected: &KarsEval) -> anyhow::Result<()> {
    identity(&current.metadata)?;
    ensure!(
        current.metadata.uid == expected.metadata.uid
            && current.namespace() == expected.namespace()
            && current.name_any() == expected.name_any()
            && current.metadata.generation == expected.metadata.generation
            && current.metadata.deletion_timestamp.is_none()
            && serde_json::to_value(&current.spec)? == serde_json::to_value(&expected.spec)?,
        "Eval identity/spec changed"
    );
    for key in [
        crate::kars_eval::ANNOTATION_RUN_NOW,
        RUN_TOKEN,
        LAST_RUN,
        LAST_TOKEN,
    ] {
        ensure!(
            current.annotations().get(key) == expected.annotations().get(key),
            "Eval request changed"
        );
    }
    Ok(())
}

pub(super) fn contains(actual: &Value, expected: &Value) -> bool {
    match expected {
        Value::Object(expected) => actual.as_object().is_some_and(|actual| {
            expected.iter().all(|(key, value)| {
                actual
                    .get(key)
                    .is_some_and(|actual| contains(actual, value))
            })
        }),
        Value::Array(expected) => actual.as_array().is_some_and(|actual| {
            actual.len() >= expected.len()
                && actual.iter().zip(expected).all(|(a, b)| contains(a, b))
        }),
        _ => actual == expected,
    }
}

#[derive(Clone)]
pub(super) struct Intent {
    pub eval_uid: String,
    pub generation: i64,
    pub digest: String,
    pub corpus_digest: String,
    pub target_uid: Option<String>,
    target_digest: Option<String>,
    pub router: String,
    pub request_marker: Option<String>,
    pub pod_spec: Value,
}

impl Intent {
    pub async fn new(
        client: &Client,
        eval: &KarsEval,
        corpus: &str,
        image: &str,
        router: &str,
    ) -> anyhow::Result<Self> {
        let (uid, _) = identity(&eval.metadata)?;
        let generation = eval
            .metadata
            .generation
            .filter(|g| *g > 0)
            .context("Eval generation missing")?;
        let (target_uid, target_digest) = target_identity(client, eval).await?;
        let pod_spec = super::runner::runner_pod_spec_json(
            &eval.name_any(),
            &format!("karseval-{}-corpus", eval.name_any()),
            image,
            router,
            "",
        );
        let digest = content_digest(&serde_json::to_vec(&(
            namespace(eval)?,
            uid,
            generation,
            &eval.spec,
            corpus,
            image,
            router,
            &target_uid,
            &target_digest,
            &pod_spec,
        ))?);
        Ok(Self {
            eval_uid: uid.into(),
            generation,
            digest,
            corpus_digest: corpus.into(),
            target_uid,
            target_digest,
            router: router.into(),
            request_marker: eval.annotations().get(LAST_TOKEN).cloned(),
            pod_spec,
        })
    }

    pub fn annotations(&self) -> Value {
        json!({INTENT: self.digest, CORPUS: self.corpus_digest})
    }

    pub fn matches(&self, meta: &ObjectMeta) -> bool {
        meta.annotations.as_ref().is_some_and(|annotations| {
            annotations.get(INTENT) == Some(&self.digest)
                && annotations.get(CORPUS) == Some(&self.corpus_digest)
        })
    }

    pub async fn revalidate(&self, client: &Client, eval: &KarsEval) -> anyhow::Result<KarsEval> {
        let current = Api::<KarsEval>::namespaced(client.clone(), namespace(eval)?)
            .get(&eval.name_any())
            .await
            .context("re-read current Eval intent")?;
        same_eval(&current, eval)?;
        ensure!(
            current.metadata.uid.as_deref() == Some(self.eval_uid.as_str())
                && current.metadata.generation == Some(self.generation),
            "Eval intent changed"
        );
        ensure!(
            target_identity(client, eval).await?
                == (self.target_uid.clone(), self.target_digest.clone()),
            "evaluation target identity/spec changed"
        );
        identity(&current.metadata)?;
        Ok(current)
    }
}

async fn target_identity(
    client: &Client,
    eval: &KarsEval,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let target = Api::<KarsSandbox>::namespaced(client.clone(), namespace(eval)?)
        .get_opt(&eval.spec.target_sandbox_ref.name)
        .await
        .context("read evaluation target")?;
    let scope = target
        .map(|target| {
            ensure!(
                target.namespace() == eval.namespace()
                    && target.metadata.deletion_timestamp.is_none(),
                "evaluation target scope changed"
            );
            let uid = identity(&target.metadata)?.0.to_owned();
            let digest = content_digest(&serde_json::to_vec(&(
                &uid,
                target.metadata.generation,
                &target.spec,
            ))?);
            Ok::<_, anyhow::Error>((uid, digest))
        })
        .transpose()?;
    Ok(scope.map_or((None, None), |(uid, digest)| (Some(uid), Some(digest))))
}

pub(super) async fn claim_trigger(api: &Api<KarsEval>, eval: &KarsEval) -> anyhow::Result<bool> {
    let requested = eval
        .annotations()
        .get(crate::kars_eval::ANNOTATION_RUN_NOW)
        .map(String::as_str)
        == Some("true");
    if let Some(token) = eval.annotations().get(RUN_TOKEN) {
        ensure!(
            token.strip_prefix("sha256:").is_some_and(
                |value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            ),
            "invalid claimed run token"
        );
        if !requested {
            let (uid, rv) = identity(&eval.metadata)?;
            api.patch(
                &eval.name_any(),
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata":{"uid":uid,"resourceVersion":rv,"annotations":{RUN_TOKEN:null}},
                })),
            )
            .await
            .context("clear cancelled request token")?;
            return Ok(true);
        }
        return Ok(false);
    }
    if !requested {
        return Ok(false);
    }
    let (uid, rv) = identity(&eval.metadata)?;
    let token = content_digest(&serde_json::to_vec(&(uid, rv))?);
    api.patch(
        &eval.name_any(),
        &PatchParams::default(),
        &Patch::Merge(json!({
            "metadata":{"uid":uid,"resourceVersion":rv,"annotations":{RUN_TOKEN:token}},
        })),
    )
    .await
    .context("claim run request before workload creation")?;
    Ok(true)
}

pub(super) async fn acknowledge_trigger(
    api: &Api<KarsEval>,
    eval: &KarsEval,
    job_name: &str,
) -> anyhow::Result<KarsEval> {
    let (uid, rv) = identity(&eval.metadata)?;
    api.patch(
        &eval.name_any(),
        &PatchParams::default(),
        &Patch::Merge(json!({
            "metadata":{"uid":uid,"resourceVersion":rv,"annotations":{
                crate::kars_eval::ANNOTATION_RUN_NOW:null, RUN_TOKEN:null, LAST_RUN:job_name,
                LAST_TOKEN:eval.annotations().get(RUN_TOKEN),
            }},
        })),
    )
    .await
    .context("acknowledge created run with original UID/resourceVersion")
}

pub(super) fn owner(eval: &KarsEval) -> anyhow::Result<Value> {
    let (uid, _) = identity(&eval.metadata)?;
    Ok(super::karseval_owner_refs(&eval.name_any(), uid))
}

pub(super) fn terminal(
    job: &k8s_openapi::api::batch::v1::Job,
) -> anyhow::Result<Option<(bool, String)>> {
    let Some(status) = &job.status else {
        return Ok(None);
    };
    let terminal: Vec<_> = status
        .conditions
        .iter()
        .flatten()
        .filter(|condition| {
            condition.status == "True" && matches!(condition.type_.as_str(), "Complete" | "Failed")
        })
        .collect();
    ensure!(terminal.len() <= 1, "contradictory terminal Job conditions");
    terminal
        .first()
        .map(|condition| {
            let at = condition
                .last_transition_time
                .as_ref()
                .context("terminal Job has no completion time")?;
            Ok((condition.type_ == "Complete", at.0.to_string()))
        })
        .transpose()
}
