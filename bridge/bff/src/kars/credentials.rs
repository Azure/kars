// Governed credential adapter. Values stay in native, operator-authorized Secrets.

use super::cluster::Cluster;
pub use super::credential_contract::{
    CredentialBindings, Grant, Identity, Legacy, Selection, SourceState, Store, Target,
};
use super::credential_review::{
    CredentialReview, CredentialWriteFailure, ReviewedWrite, StoredSource,
};
pub use super::credential_transport::failure;
use super::credential_transport::{identity, object_api, safe};
use k8s_openapi::api::core::v1::{Namespace, Secret};
use kube::{
    Api, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const GRANT: &str = "workspace";
const PREFIX: &str = "kars-credential-input-";
const REMOVED_KEYS: &str = "kars.azure.com/credential-removed-keys";

struct AgentCredentialWrite<'a> {
    namespace: &'a str,
    kind: &'a str,
    name: &'a str,
    expected_target_uid: Option<&'a str>,
    set: BTreeMap<String, String>,
    remove: Vec<String>,
}

pub fn input_name(kind: &str, name: &str) -> Result<String, kube::Error> {
    match kind {
        "Workspace" => Ok(format!("{PREFIX}workspace")),
        "KarsSandbox" => Ok(format!("{PREFIX}sandbox-{name}")),
        "KarsTask" => Ok(format!("{PREFIX}task-{name}")),
        "KarsTeam" => Ok(format!("{PREFIX}team-{name}")),
        _ => Err(failure(
            "target kind must be KarsSandbox, KarsTask or KarsTeam",
        )),
    }
}
impl Cluster {
    pub fn core_namespace(&self) -> String {
        std::env::var("BRIDGE_CORE_NAMESPACE").unwrap_or_else(|_| "kars-system".into())
    }
    pub fn integration_namespace(&self) -> String {
        std::env::var("BRIDGE_INSTALL_NAMESPACE").unwrap_or_else(|_| self.core_namespace())
    }
    fn operator_store_namespace(&self, requested: &str, name: &str) -> String {
        if [
            "kars-inference-providers",
            "kars-foundry-credentials",
            "kars-github-app",
            "kars-github-connection",
            "kars-credential-controller-settings",
        ]
        .contains(&name)
            || name.starts_with("kars-provider-")
        {
            self.core_namespace()
        } else {
            requested.into()
        }
    }
    pub async fn credential_grant(&self, namespace: &str) -> Result<Grant, kube::Error> {
        let document = object_api(self, namespace, "KarsCredentialGrant")
            .get(GRANT)
            .await
            .map_err(|e| safe("Read workspace credential grant", e))?;
        if document.metadata.deletion_timestamp.is_some()
            || document.data["spec"]["enabled"] != true
            || document.data["status"]["phase"] != "Ready"
            || document.data["status"]["observedGeneration"] != json!(document.metadata.generation)
        {
            return Err(failure(
                "Workspace credential authority is unready; an operator must enroll or repair it before credential changes",
            ));
        }
        let namespace_object = Api::<Namespace>::all(self.client.clone())
            .get(namespace)
            .await
            .map_err(|e| safe("Read credential workspace identity", e))?;
        if document.data["spec"]["workspaceUid"] != json!(namespace_object.metadata.uid)
            || namespace_object.metadata.deletion_timestamp.is_some()
        {
            return Err(failure("Credential workspace identity changed"));
        }
        let parse = |value: &Value| value.clone();
        Ok(Grant {
            identity: identity(&document)?,
            agent_keys: serde_json::from_value(parse(&document.data["spec"]["agentKeys"]))
                .unwrap_or_default(),
            stores: serde_json::from_value(parse(&document.data["spec"]["integrationStores"]))
                .map_err(|_| failure("Credential store grant is malformed"))?,
            sources: serde_json::from_value(parse(&document.data["status"]["sources"]))
                .map_err(|_| failure("Credential metadata inventory is malformed"))?,
            legacy: serde_json::from_value(parse(&document.data["status"]["legacySources"]))
                .unwrap_or_default(),
            reviewed_legacy: serde_json::from_value(parse(&document.data["spec"]["legacyImports"]))
                .unwrap_or_default(),
            document,
        })
    }

    pub async fn credential_target(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
    ) -> Result<Option<Target>, kube::Error> {
        input_name(kind, name)?;
        let Some(object) = object_api(self, namespace, kind)
            .get_opt(name)
            .await
            .map_err(|e| safe("Read credential target", e))?
        else {
            return Ok(None);
        };
        if object.metadata.deletion_timestamp.is_some() {
            return Err(failure("Credential target is terminating"));
        }
        Ok(Some(Target {
            kind: kind.into(),
            namespace: namespace.into(),
            name: name.into(),
            uid: identity(&object)?.uid,
        }))
    }

    pub async fn integration_store(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<(Store, Secret), kube::Error> {
        let namespace = self.operator_store_namespace(namespace, name);
        let grant = self.credential_grant(&namespace).await?;
        let store=grant.stores.into_iter().find(|store|store.secret.name==name)
            .ok_or_else(||failure("Integration store is not explicitly enrolled; no broad Secret fallback is allowed"))?;
        let secret = Api::<Secret>::namespaced(self.client.clone(), &namespace)
            .get(name)
            .await
            .map_err(|e| safe("Read enrolled integration store", e))?;
        if secret.uid().as_deref() != Some(store.secret.uid.as_str())
            || secret.type_.as_deref() != Some("Opaque")
            || secret.metadata.deletion_timestamp.is_some()
        {
            return Err(failure(
                "Integration store UID/type changed; replacement preserved",
            ));
        }
        Ok((store, secret))
    }

    pub async fn patch_credential_keys(
        &self,
        namespace: &str,
        name: &str,
        uid: &str,
        version: &str,
        set: &BTreeMap<String, String>,
        remove: &[String],
    ) -> Result<Secret, kube::Error> {
        let api: Api<Secret> = Api::namespaced(self.client.clone(), namespace);
        let current = api
            .get(name)
            .await
            .map_err(|e| safe("Read exact credential collection", e))?;
        if current.uid().as_deref() != Some(uid)
            || current.resource_version().as_deref() != Some(version)
        {
            return Err(kube::Error::Api(kube::core::ErrorResponse {
                status: "Failure".into(),
                reason: "Conflict".into(),
                message: "Credential UID/resourceVersion changed before mutation".into(),
                code: 409,
            }));
        }
        let mut data = serde_json::to_value(current.data.unwrap_or_default())
            .map_err(|_| failure("Credential data encoding failed"))?;
        for (key, value) in set {
            if value.contains('\0') {
                return Err(failure("Credential values must not contain NUL"));
            }
            data[key] = serde_json::to_value(k8s_openapi::ByteString(value.as_bytes().to_vec()))
                .map_err(|_| failure("Credential encoding failed"))?;
        }
        for key in remove {
            data.as_object_mut()
                .ok_or_else(|| failure("Credential data is not an object"))?
                .remove(key);
        }
        let mut operations = vec![
            json!({"op":"test","path":"/metadata/uid","value":uid}),
            json!({"op":"test","path":"/metadata/resourceVersion","value":version}),
            json!({"op":"add","path":"/data","value":data}),
        ];
        if name.starts_with(PREFIX) {
            let mut annotations = current.metadata.annotations.unwrap_or_default();
            let prior: Vec<String> = annotations
                .get(REMOVED_KEYS)
                .map(|raw| serde_json::from_str(raw))
                .transpose()
                .map_err(|_| failure("Credential removal intent is malformed"))?
                .unwrap_or_default();
            let mut removed = prior.into_iter().collect::<BTreeSet<_>>();
            for key in set.keys() {
                removed.remove(key);
            }
            removed.extend(remove.iter().cloned());
            if removed.len() > 128 {
                return Err(failure("Credential removal intent exceeds its bound"));
            }
            annotations.insert(
                REMOVED_KEYS.into(),
                serde_json::to_string(&removed)
                    .map_err(|_| failure("Credential removal intent serialization failed"))?,
            );
            operations.push(json!({"op":"add","path":"/metadata/annotations","value":annotations}));
        }
        let patch: json_patch::Patch = serde_json::from_value(json!(operations))
            .map_err(|_| failure("Credential patch serialization failed"))?;
        api.patch(name, &PatchParams::default(), &Patch::Json::<Secret>(patch))
            .await
            .map_err(|e| safe("Apply UID/resourceVersion-fenced credential keys", e))
    }

    pub async fn mutate_integration(
        &self,
        namespace: &str,
        name: &str,
        mutate: impl Fn(&mut BTreeMap<String, String>),
    ) -> Result<(), kube::Error> {
        let namespace = self.operator_store_namespace(namespace, name);
        for _ in 0..6 {
            let (_, secret) = self.integration_store(&namespace, name).await?;
            let before = secret
                .data
                .as_ref()
                .into_iter()
                .flatten()
                .map(|(key, value)| {
                    String::from_utf8(value.0.clone()).map(|value| (key.clone(), value))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()
                .map_err(|_| failure("Integration store contains non-UTF8 values"))?;
            let mut after = before.clone();
            mutate(&mut after);
            let removed = before
                .keys()
                .filter(|key| !after.contains_key(*key))
                .cloned()
                .collect::<Vec<_>>();
            let changed = after
                .into_iter()
                .filter(|(key, value)| before.get(key) != Some(value))
                .collect::<BTreeMap<_, _>>();
            if changed.is_empty() && removed.is_empty() {
                return Ok(());
            }
            let uid = secret
                .uid()
                .ok_or_else(|| failure("Integration UID missing"))?;
            let rv = secret
                .resource_version()
                .ok_or_else(|| failure("Integration resourceVersion missing"))?;
            match self
                .patch_credential_keys(&namespace, name, &uid, &rv, &changed, &removed)
                .await
            {
                Ok(_) => return Ok(()),
                Err(kube::Error::Api(error)) if error.code == 409 || error.code == 422 => continue,
                Err(error) => return Err(error),
            }
        }
        Err(failure(
            "Concurrent integration update did not converge; no unrelated keys were overwritten",
        ))
    }

    pub async fn write_agent_credentials(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
        expected_target_uid: Option<&str>,
        set: BTreeMap<String, String>,
        remove: Vec<String>,
    ) -> Result<Value, kube::Error> {
        let mut stored = None;
        let mut attempted = false;
        self.apply_agent_credentials(
            AgentCredentialWrite {
                namespace,
                kind,
                name,
                expected_target_uid,
                set,
                remove,
            },
            None,
            &mut stored,
            &mut attempted,
        )
        .await
    }

    pub async fn write_reviewed_agent_credentials(
        &self,
        reviewed: &ReviewedWrite,
        value: String,
    ) -> Result<Value, Box<CredentialWriteFailure>> {
        let review = &reviewed.review;
        let mut stored = reviewed.stored.clone();
        let mut attempted = false;
        let result = self
            .apply_agent_credentials(
                AgentCredentialWrite {
                    namespace: &review.target.namespace,
                    kind: &review.target.kind,
                    name: &review.target.name,
                    expected_target_uid: review.target.uid.as_deref(),
                    set: BTreeMap::from([(review.key.clone(), value)]),
                    remove: Vec::new(),
                },
                Some(reviewed),
                &mut stored,
                &mut attempted,
            )
            .await;
        result.map_err(|error| {
            Box::new(CredentialWriteFailure {
                error,
                stored,
                write_attempted: attempted,
            })
        })
    }

    async fn apply_agent_credentials(
        &self,
        input: AgentCredentialWrite<'_>,
        reviewed: Option<&ReviewedWrite>,
        stored: &mut Option<StoredSource>,
        attempted: &mut bool,
    ) -> Result<Value, kube::Error> {
        let AgentCredentialWrite {
            namespace,
            kind,
            name,
            expected_target_uid,
            set,
            remove,
        } = input;
        let grant = self.credential_grant(namespace).await?;
        if let Some(reviewed) = reviewed {
            self.check_reviewed_grant(&reviewed.review, &grant)?;
            self.check_credential_review(&reviewed.review).await?;
        }
        if set.values().any(|value| value.contains('\0'))
            || set.values().map(String::len).sum::<usize>() > 131_072
        {
            return Err(failure(
                "Credential values must be UTF-8 without NUL and within 128 KiB",
            ));
        }
        let target = if kind == "Workspace" {
            None
        } else {
            self.credential_target(namespace, kind, name).await?
        };
        if target.is_none()
            && kind != "Workspace"
            && Api::<Namespace>::all(self.client.clone())
                .get_opt(&format!("kars-{name}"))
                .await
                .map_err(|e| safe("Preflight prelaunch runtime namespace", e))?
                .is_some()
        {
            return Err(failure(
                "An existing runtime namespace makes prelaunch migration ambiguous; stage the real paused target and review its UID and legacy keys first",
            ));
        }
        if expected_target_uid
            .is_some_and(|uid| target.as_ref().is_none_or(|target| target.uid != uid))
        {
            return Err(failure("Reviewed credential target UID changed"));
        }
        let source_name = input_name(kind, name)?;
        let legacy = grant
            .legacy
            .iter()
            .filter(|entry| entry.source_name == source_name && entry.target == target)
            .collect::<Vec<_>>();
        if legacy
            .iter()
            .any(|entry| !grant.reviewed_legacy.contains(entry))
        {
            return Err(failure(
                "Existing legacy credentials require read-only operator UID/resourceVersion/key-name review before migration",
            ));
        }
        if set
            .keys()
            .chain(remove.iter())
            .any(|key| !grant.approves_agent_key(key))
        {
            return Err(failure(
                "Credential key needs an explicit operator agent-key grant",
            ));
        }
        let prior = grant
            .sources
            .iter()
            .find(|source| source.name == source_name);
        if prior.is_some_and(|source| {
            source
                .target
                .as_ref()
                .is_some_and(|owner| Some(owner) != target.as_ref())
        }) {
            return Err(failure("Credential source belongs to another target UID"));
        }
        let mut keys = prior.map(|s| s.keys.clone()).unwrap_or_default();
        keys.extend(set.keys().cloned());
        keys.extend(remove.iter().cloned());
        keys.extend(legacy.iter().flat_map(|entry| {
            entry
                .keys
                .iter()
                .filter(|key| key.as_str() != "TEAMS_ENABLED")
                .cloned()
        }));
        keys.sort();
        keys.dedup();
        let api: Api<Secret> = Api::namespaced(self.client.clone(), namespace);
        let workspace_plan = if kind == "Workspace" {
            use super::workspace_credential_plan::WorkspaceSource;
            let source = match prior {
                Some(prior) => WorkspaceSource::Existing(Identity {
                    name: source_name.clone(),
                    uid: prior.uid.clone(),
                }),
                // New names are CREATE-only until core enrollment grants GET.
                // Existence is decided by exclusive CREATE, never by a 403.
                None => WorkspaceSource::Create {
                    name: source_name.clone(),
                },
            };
            Some(
                self.plan_workspace_credentials(namespace, &grant.identity, source, keys.clone())
                    .await?,
            )
        } else {
            None
        };
        if let Some(reviewed) = reviewed {
            self.check_credential_review(&reviewed.review).await?;
        }
        let mut written = if let Some(resume) = reviewed.and_then(|review| review.stored.as_ref()) {
            let metadata = api
                .get_metadata(&source_name)
                .await
                .map_err(|error| safe("Verify acknowledged continuation source", error))?;
            let current = StoredSource::from_metadata(&metadata.metadata)?;
            if &current != resume || metadata.metadata.deletion_timestamp.is_some() {
                return Err(failure(
                    "Continuation source changed; no source write or binding was attempted",
                ));
            }
            current
        } else if let Some(prior) = prior {
            let version = if prior.phase == "Blocked" {
                prior.resource_version.clone()
            } else {
                let meta = api
                    .get_metadata(&source_name)
                    .await
                    .map_err(|e| safe("Refresh source metadata", e))?;
                if meta.uid().as_deref() != Some(prior.uid.as_str()) {
                    return Err(failure("Source UID changed"));
                }
                meta.resource_version()
                    .ok_or_else(|| failure("Source resourceVersion missing"))?
            };
            if reviewed.is_some_and(|review| {
                review.review.source.version.as_deref() != Some(version.as_str())
            }) {
                return Err(failure("Source version changed after review"));
            }
            *attempted = true;
            let written = self
                .patch_credential_keys(namespace, &source_name, &prior.uid, &version, &set, &remove)
                .await?;
            StoredSource::from_metadata(&written.metadata)?
        } else {
            if let Some(target) = &target {
                let object = object_api(self, namespace, kind)
                    .get(name)
                    .await
                    .map_err(|e| safe("Inspect existing target binding", e))?;
                let configured = if kind == "KarsSandbox" {
                    &object.data["spec"]["credentialBindings"]
                } else {
                    &object.data["spec"]["blueprint"]["credentialBindings"]
                };
                if configured["sources"].as_array().is_some_and(|sources| {
                    sources
                        .iter()
                        .any(|source| source["source"]["name"] == source_name)
                }) {
                    return Err(failure(
                        "A previously referenced source disappeared; explicit operator replacement is required",
                    ));
                }
                if object.uid().as_deref() != Some(target.uid.as_str()) {
                    return Err(failure("Target changed during source preflight"));
                }
            }
            let mut annotations = json!({
                "kars.azure.com/credential-purpose":"agent-input-v2",
                "kars.azure.com/credential-workspace":namespace,
                "kars.azure.com/credential-target-kind":kind,
                "kars.azure.com/credential-target":name,
                "kars.azure.com/credential-binding-intent":"explicit-reference-v2",
                "kars.azure.com/credential-grant-uid":grant.identity.uid,
                REMOVED_KEYS:serde_json::to_string(&remove.iter().cloned().collect::<BTreeSet<_>>())
                    .map_err(|_|failure("Credential removal intent serialization failed"))?,
            });
            if let Some(target) = &target {
                annotations["kars.azure.com/credential-target-uid"] = target.uid.clone().into();
            }
            let values = set
                .iter()
                .filter(|(key, _)| !remove.contains(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>();
            let secret:Secret=serde_json::from_value(json!({"apiVersion":"v1","kind":"Secret","type":"Opaque",
                "metadata":{"name":source_name,"namespace":namespace,"annotations":annotations},"stringData":values}))
                .map_err(|_|failure("Credential source serialization failed"))?;
            *attempted = true;
            let written = api
                .create(&PostParams::default(), &secret)
                .await
                .map_err(|e| safe("Create unbound credential source without adoption", e))?;
            StoredSource::from_metadata(&written.metadata)?
        };
        if written.name != source_name {
            return Err(failure("Stored source name changed"));
        }
        if reviewed.is_some_and(|review| {
            review
                .review
                .source
                .uid
                .as_ref()
                .is_some_and(|uid| uid != &written.uid)
        }) {
            return Err(failure("Reviewed source UID changed after its write"));
        }
        *stored = Some(written.clone());
        let source = Identity {
            name: source_name.clone(),
            uid: written.uid.clone(),
        };
        if let Some(plan) = workspace_plan {
            self.await_source_metadata(namespace, &source).await?;
            let current = api
                .get_metadata(&source_name)
                .await
                .map_err(|e| safe("Verify written workspace source before binding", e))?;
            if current.uid().as_deref() != Some(source.uid.as_str())
                || current.metadata.resource_version.as_deref() != Some(written.version.as_str())
                || current.metadata.deletion_timestamp.is_some()
            {
                return Err(failure(
                    "Workspace source changed after its write; consumer bindings were not applied",
                ));
            }
            self.apply_workspace_plan(plan, &source).await?;
        }
        if let Some(reviewed) = reviewed {
            self.await_source_metadata(namespace, &source).await?;
            written = self
                .check_written_credential_review(&reviewed.review, &written)
                .await?;
            *stored = Some(written.clone());
        }
        let bound = if let Some(target) = &target {
            Some(
                self.bind_credential_selection_reviewed(
                    target,
                    &grant.identity,
                    Selection {
                        scope: if kind == "KarsTeam" { "team" } else { "target" }.into(),
                        source: source.clone(),
                        keys,
                        owner: Some(target.clone()),
                    },
                    reviewed.map(|review| &review.review),
                )
                .await?,
            )
        } else {
            None
        };
        self.await_source_metadata(namespace, &source).await?;
        if let Some(reviewed) = reviewed {
            self.check_completed_credential_review(&reviewed.review, &written, bound.as_ref())
                .await?;
        }
        Ok(
            json!({"stored":true,"source":source,"namespace":namespace,"target":target,
            "resourceVersion":written.version,
            "phase":if target.is_some() {"AwaitingController"} else {"Unbound"},
            "note":"Stored in the governed workspace source. No runtime namespace was created; delivery requires the current controller's UID-bound acknowledgement."}),
        )
    }

    pub async fn bind_credential_selection(
        &self,
        target: &Target,
        grant: &Identity,
        selection: Selection,
    ) -> Result<(), kube::Error> {
        self.bind_credential_selection_reviewed(target, grant, selection, None)
            .await
            .map(|_| ())
    }

    async fn bind_credential_selection_reviewed(
        &self,
        target: &Target,
        grant: &Identity,
        selection: Selection,
        review: Option<&CredentialReview>,
    ) -> Result<kube::api::DynamicObject, kube::Error> {
        let api = object_api(self, &target.namespace, &target.kind);
        let object = api
            .get(&target.name)
            .await
            .map_err(|e| safe("Read target before credential binding", e))?;
        if let Some(review) = review {
            self.check_reviewed_target(review, &object)?;
        }
        let Some(spec) =
            super::credential_targets::planned_selection(&object, target, grant, selection)?
        else {
            return Ok(object);
        };
        let result = api.patch(&target.name,&PatchParams::default(),&Patch::Merge(json!({
            "metadata":{"uid":target.uid,"resourceVersion":object.metadata.resource_version},"spec":spec,
        }))).await.map_err(|e|safe("Bind the captured target and source UIDs",e))?;
        if review.is_some() {
            self.check_bound_credential_target(&object, &spec, &result)?;
        }
        Ok(result)
    }

    pub(super) async fn await_source_metadata(
        &self,
        namespace: &str,
        source: &Identity,
    ) -> Result<(), kube::Error> {
        for _ in 0..120 {
            let grant = self.credential_grant(namespace).await?;
            if let Some(observed) = grant
                .sources
                .iter()
                .find(|entry| entry.name == source.name && entry.uid == source.uid)
            {
                if observed.phase == "Blocked" {
                    return Err(failure(&observed.reason));
                }
                let current = Api::<Secret>::namespaced(self.client.clone(), namespace)
                    .get_metadata(&source.name)
                    .await
                    .map_err(|e| safe("Read observed source metadata", e))?;
                if current.uid().as_deref() != Some(source.uid.as_str()) {
                    return Err(failure("Source was replaced during observation"));
                }
                if current.resource_version().as_deref() == Some(observed.resource_version.as_str())
                {
                    return Ok(());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        Err(failure(
            "Source was stored but the controller has not acknowledged its current UID/resourceVersion",
        ))
    }

    pub async fn configured_channel_keys(
        &self,
        namespace: &str,
        target: Option<&Target>,
    ) -> Result<Vec<String>, kube::Error> {
        let grant = self.credential_grant(namespace).await?;
        let mut keys = BTreeSet::new();
        if let Some(workspace) = grant
            .sources
            .iter()
            .find(|source| source.name == format!("{PREFIX}workspace"))
        {
            if workspace.phase == "Blocked" {
                return Err(failure(&workspace.reason));
            }
            keys.extend(workspace.keys.iter().cloned());
        }
        if let Some(target) = target {
            let object = object_api(self, namespace, &target.kind)
                .get(&target.name)
                .await
                .map_err(|e| safe("Read channel target", e))?;
            if object.uid().as_deref() != Some(target.uid.as_str()) {
                return Err(failure("Channel target UID changed"));
            }
            let bindings = &object.data["spec"]["blueprint"]["credentialBindings"];
            if let Some(sources) = bindings["sources"].as_array() {
                for selection in sources {
                    let uid = selection["source"]["uid"]
                        .as_str()
                        .ok_or_else(|| failure("Channel source UID missing"))?;
                    let source = grant
                        .sources
                        .iter()
                        .find(|source| source.uid == uid)
                        .ok_or_else(|| {
                            failure("Channel source is missing or has not been observed")
                        })?;
                    if source.phase == "Blocked" {
                        return Err(failure(&source.reason));
                    }
                    for key in selection["keys"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        if source.keys.iter().any(|present| present == key) {
                            keys.insert(key.into());
                        } else {
                            keys.remove(key);
                        }
                    }
                }
            }
        }
        Ok(keys.into_iter().collect())
    }

    pub async fn write_controller_environment(
        &self,
        changes: Vec<Value>,
    ) -> Result<(), kube::Error> {
        let namespace = self.core_namespace();
        let grant = self.credential_grant(&namespace).await?;
        if grant.document.data["spec"]["controller"]["name"] != "kars-controller"
            || grant.document.data["spec"]["controller"]["uid"]
                .as_str()
                .is_none_or(str::is_empty)
        {
            return Err(failure(
                "The controller Deployment UID must be enrolled before provider configuration",
            ));
        }
        let mut incoming = Vec::new();
        for entry in changes {
            let name = entry["name"]
                .as_str()
                .ok_or_else(|| failure("Controller setting name missing"))?;
            if entry["$patch"] == "delete" {
                incoming.push(json!({"name":name,"remove":true}));
            } else if let Some(secret) = entry.get("valueFrom").and_then(|v| v.get("secretKeyRef"))
            {
                let secret_name = secret["name"]
                    .as_str()
                    .ok_or_else(|| failure("Controller Secret reference name missing"))?;
                let store = grant
                    .stores
                    .iter()
                    .find(|store| store.secret.name == secret_name)
                    .ok_or_else(|| failure("Controller credential Secret is not enrolled"))?;
                incoming.push(json!({"name":name,"secret":{"name":secret_name,"uid":store.secret.uid,"key":secret["key"]}}));
            } else if let Some(value) = entry["value"].as_str() {
                incoming.push(json!({"name":name,"value":value}));
            } else {
                return Err(failure("Unsupported controller environment change"));
            }
        }
        self.mutate_integration(&namespace, "kars-credential-controller-settings", |keys| {
            let mut values = keys
                .get("configuration")
                .and_then(|raw| serde_json::from_str::<Vec<Value>>(raw).ok())
                .unwrap_or_default();
            for change in &incoming {
                values.retain(|existing| existing["name"] != change["name"]);
                values.push(change.clone());
            }
            keys.insert(
                "configuration".into(),
                serde_json::to_string(&values).expect("environment settings serialize"),
            );
        })
        .await
    }

    pub async fn request_teams_reconcile(
        &self,
        namespace: &str,
        gateway: &str,
        bff: &str,
    ) -> Result<(), kube::Error> {
        let grant = self.credential_grant(namespace).await?;
        let consumers = &grant.document.data["spec"]["bridgeConsumers"];
        if consumers["gateway"]["name"] != gateway || consumers["bff"]["name"] != bff {
            return Err(failure(
                "Teams Deployment identities must be enrolled; Bridge cannot patch arbitrary Deployments",
            ));
        }
        if let Some(error) = grant.document.data["status"]["integrationError"].as_str() {
            return Err(failure(error));
        }
        Ok(())
    }

    pub async fn teams_configured(&self) -> Result<bool, kube::Error> {
        let namespace = self.integration_namespace();
        let name = std::env::var("BRIDGE_TEAMS_SECRET_NAME")
            .unwrap_or_else(|_| "kars-bridge-teams".into());
        let Some(document) = object_api(self, &namespace, "KarsCredentialGrant")
            .get_opt(GRANT)
            .await
            .map_err(|e| safe("Read optional Teams authority", e))?
        else {
            return Ok(false);
        };
        if !document.data["spec"]["integrationStores"]
            .as_array()
            .is_some_and(|stores| {
                stores
                    .iter()
                    .any(|store| store["secret"]["name"] == name && store["purpose"] == "teams")
            })
        {
            return Ok(false);
        }
        let (_, secret) = self.integration_store(&namespace, &name).await?;
        Ok([
            "client-id",
            "tenant-id",
            "client-secret",
            "entra-role-map",
            "bff-internal-secret",
        ]
        .iter()
        .all(|key| {
            secret
                .data
                .as_ref()
                .and_then(|values| values.get(*key))
                .is_some_and(|value| !value.0.is_empty())
        }))
    }
}
