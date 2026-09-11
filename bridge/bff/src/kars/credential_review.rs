use super::{
    cluster::Cluster,
    credential_contract::{Grant, Identity, Selection, Target},
    credential_targets::{planned_selection, reviewed_bindings},
    credential_transport::{failure, object_api, safe},
    credentials::input_name,
};
use k8s_openapi::{
    api::core::v1::{Namespace, Secret},
    apimachinery::pkg::apis::meta::v1::ObjectMeta,
};
use kube::{Api, ResourceExt, api::DynamicObject};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceReview {
    pub name: String,
    pub uid: Option<String>,
    pub version: Option<String>,
    pub metadata_digest: Option<String>,
    pub keys: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetReview {
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub uid: Option<String>,
    pub generation: Option<i64>,
    pub version: Option<String>,
    pub intent: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantReview {
    pub uid: String,
    pub generation: i64,
    pub version: String,
    pub intent: String,
    pub workspace_uid: String,
    pub legacy_inventory: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialReview {
    pub target: TargetReview,
    pub grant: GrantReview,
    pub source: SourceReview,
    pub key: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredSource {
    pub name: String,
    pub uid: String,
    pub version: String,
    pub metadata_digest: String,
}

pub struct ReviewedWrite {
    pub review: CredentialReview,
    pub stored: Option<StoredSource>,
}

pub struct CredentialWriteFailure {
    pub error: kube::Error,
    pub stored: Option<StoredSource>,
    pub write_attempted: bool,
}

fn digest(value: &impl Serialize) -> Result<String, kube::Error> {
    let mut value =
        serde_json::to_value(value).map_err(|_| failure("Credential review encoding failed"))?;
    value.sort_all_objects();
    let bytes =
        serde_json::to_vec(&value).map_err(|_| failure("Credential review encoding failed"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn intent(object: &DynamicObject) -> Result<String, kube::Error> {
    let mut value =
        serde_json::to_value(object).map_err(|_| failure("Credential intent encoding failed"))?;
    value
        .as_object_mut()
        .ok_or_else(|| failure("Credential target is malformed"))?
        .remove("status");
    let metadata = value["metadata"]
        .as_object_mut()
        .ok_or_else(|| failure("Credential metadata is malformed"))?;
    metadata.remove("resourceVersion");
    if let Some(Value::Array(fields)) = metadata.get_mut("managedFields") {
        fields.retain(|field| field["subresource"] != "status");
        if fields.is_empty() {
            metadata.remove("managedFields");
        }
    }
    digest(&value)
}

impl StoredSource {
    pub(super) fn from_metadata(metadata: &ObjectMeta) -> Result<Self, kube::Error> {
        Ok(Self {
            name: metadata
                .name
                .clone()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| failure("Stored source name missing"))?,
            uid: metadata
                .uid
                .clone()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| failure("Stored source UID missing"))?,
            version: metadata
                .resource_version
                .clone()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| failure("Stored source version missing"))?,
            metadata_digest: digest(metadata)?,
        })
    }

    pub fn matches(&self, source: &SourceReview) -> bool {
        source.name == self.name
            && source.uid.as_ref() == Some(&self.uid)
            && source.version.as_ref() == Some(&self.version)
            && source.metadata_digest.as_ref() == Some(&self.metadata_digest)
    }
}

impl CredentialReview {
    fn same_authority(&self, current: &Self) -> bool {
        let mut target = current.target.clone();
        target.version.clone_from(&self.target.version);
        let mut grant = current.grant.clone();
        grant.version.clone_from(&self.grant.version);
        self.key == current.key && target == self.target && grant == self.grant
    }

    pub fn permits_refresh(&self, current: &Self, stored: &StoredSource) -> bool {
        let mut keys = self.source.keys.clone();
        keys.push(self.key.clone());
        keys.sort();
        keys.dedup();
        self.same_authority(current)
            && stored.matches(&current.source)
            && current.source.keys == keys
    }

    pub fn permits_unwritten_refresh(&self, current: &Self) -> bool {
        self.same_authority(current) && self.source == current.source
    }
}

fn grant_review(grant: &Grant) -> Result<GrantReview, kube::Error> {
    Ok(GrantReview {
        uid: grant.identity.uid.clone(),
        generation: grant
            .document
            .metadata
            .generation
            .ok_or_else(|| failure("Grant generation missing"))?,
        version: grant
            .document
            .resource_version()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| failure("Grant version missing"))?,
        intent: intent(&grant.document)?,
        workspace_uid: grant.document.data["spec"]["workspaceUid"]
            .as_str()
            .ok_or_else(|| failure("Workspace UID missing"))?
            .into(),
        legacy_inventory: digest(&grant.legacy)?,
    })
}

fn target_review(
    kind: &str,
    namespace: &str,
    name: &str,
    object: Option<&DynamicObject>,
) -> Result<TargetReview, kube::Error> {
    let mut result = TargetReview {
        kind: kind.into(),
        namespace: namespace.into(),
        name: name.into(),
        uid: None,
        generation: None,
        version: None,
        intent: None,
    };
    if let Some(object) = object {
        if object.metadata.deletion_timestamp.is_some()
            || object.namespace().as_deref() != Some(namespace)
            || object.name_any() != name
            || object.types.as_ref().is_none_or(|types| {
                types.api_version != "kars.azure.com/v1alpha1" || types.kind != kind
            })
            || !object.data["spec"].is_object()
        {
            return Err(failure("Credential target identity changed"));
        }
        result.uid = Some(
            object
                .uid()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| failure("Target UID missing"))?,
        );
        result.generation = Some(
            object
                .metadata
                .generation
                .ok_or_else(|| failure("Target generation missing"))?,
        );
        result.version = Some(
            object
                .resource_version()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| failure("Target version missing"))?,
        );
        result.intent = Some(intent(object)?);
    }
    Ok(result)
}

impl Cluster {
    pub async fn review_credentials(
        &self,
        namespace: &str,
        kind: &str,
        name: &str,
        key: &str,
    ) -> Result<CredentialReview, kube::Error> {
        let source_name = input_name(kind, name)?;
        let grant = self.credential_grant(namespace).await?;
        if !grant.approves_agent_key(key) {
            return Err(failure("Credential key is outside the operator grant"));
        }
        let object = object_api(self, namespace, kind)
            .get_opt(name)
            .await
            .map_err(|error| safe("Review credential target", error))?;
        let target = target_review(kind, namespace, name, object.as_ref())?;
        if target.uid.is_none()
            && Api::<Namespace>::all(self.client.clone())
                .get_opt(&format!("kars-{name}"))
                .await
                .map_err(|error| safe("Review staging namespace", error))?
                .is_some()
        {
            return Err(failure(
                "An existing runtime namespace prevents unbound staging review",
            ));
        }
        let owner = target.uid.as_ref().map(|uid| Target {
            kind: kind.into(),
            namespace: namespace.into(),
            name: name.into(),
            uid: uid.clone(),
        });
        let prior = grant
            .sources
            .iter()
            .find(|source| source.name == source_name);
        if prior.is_some_and(|source| {
            source.phase == "Blocked"
                || source.phase.is_empty()
                || source
                    .target
                    .as_ref()
                    .is_some_and(|target| Some(target) != owner.as_ref())
        }) {
            return Err(failure(
                "Credential source is blocked or belongs to different authority",
            ));
        }
        let mut source = SourceReview {
            name: source_name,
            uid: None,
            version: None,
            metadata_digest: None,
            keys: prior.map(|source| source.keys.clone()).unwrap_or_default(),
        };
        for legacy in grant
            .legacy
            .iter()
            .filter(|legacy| legacy.source_name == source.name && legacy.target == owner)
        {
            if !grant.reviewed_legacy.contains(legacy) {
                return Err(failure("Legacy source requires operator review"));
            }
            source.keys.extend(
                legacy
                    .keys
                    .iter()
                    .filter(|key| key.as_str() != "TEAMS_ENABLED")
                    .cloned(),
            );
        }
        source.keys.sort();
        source.keys.dedup();
        if let Some(prior) = prior {
            let metadata = Api::<Secret>::namespaced(self.client.clone(), namespace)
                .get_metadata(&source.name)
                .await
                .map_err(|error| safe("Review acknowledged source metadata", error))?;
            let stored = StoredSource::from_metadata(&metadata.metadata)?;
            if metadata.metadata.deletion_timestamp.is_some()
                || stored.name != source.name
                || metadata.metadata.namespace.as_deref() != Some(namespace)
                || stored.uid != prior.uid
                || stored.version != prior.resource_version
            {
                return Err(failure(
                    "Source metadata is not acknowledged at this UID/version",
                ));
            }
            source.uid = Some(stored.uid);
            source.version = Some(stored.version);
            source.metadata_digest = Some(stored.metadata_digest);
        }
        if let (Some(object), Some(owner)) = (object.as_ref(), owner.as_ref()) {
            let bindings = reviewed_bindings(object, owner, &grant.identity)?;
            let scope = if kind == "KarsTeam" { "team" } else { "target" };
            if bindings.sources.iter().any(|selected| {
                selected.scope == scope
                    && (selected.source.name != source.name
                        || Some(&selected.source.uid) != source.uid.as_ref()
                        || selected.owner.as_ref() != Some(owner))
            }) {
                return Err(failure(
                    "Credential source replacement requires separate operator review",
                ));
            }
            if let Some(uid) = &source.uid {
                let mut keys = source.keys.clone();
                keys.push(key.into());
                planned_selection(
                    object,
                    owner,
                    &grant.identity,
                    Selection {
                        scope: scope.into(),
                        source: Identity {
                            name: source.name.clone(),
                            uid: uid.clone(),
                        },
                        keys,
                        owner: Some(owner.clone()),
                    },
                )?;
            } else if kind == "KarsTask" && object.data["spec"]["execution"]["launch"] == true {
                return Err(failure(
                    "An active standalone Task requires explicit governed rebinding",
                ));
            }
        }
        Ok(CredentialReview {
            target,
            grant: grant_review(&grant)?,
            source,
            key: key.into(),
        })
    }

    pub(super) async fn check_credential_review(
        &self,
        reviewed: &CredentialReview,
    ) -> Result<(), kube::Error> {
        let current = self
            .review_credentials(
                &reviewed.target.namespace,
                &reviewed.target.kind,
                &reviewed.target.name,
                &reviewed.key,
            )
            .await?;
        if &current != reviewed {
            return Err(failure("Credential metadata changed since explicit review"));
        }
        Ok(())
    }

    pub(super) fn check_reviewed_grant(
        &self,
        reviewed: &CredentialReview,
        grant: &Grant,
    ) -> Result<(), kube::Error> {
        if grant_review(grant)? != reviewed.grant {
            return Err(failure("Reviewed credential grant changed"));
        }
        Ok(())
    }

    pub(super) fn check_reviewed_target(
        &self,
        reviewed: &CredentialReview,
        object: &DynamicObject,
    ) -> Result<(), kube::Error> {
        if target_review(
            &reviewed.target.kind,
            &reviewed.target.namespace,
            &reviewed.target.name,
            Some(object),
        )? != reviewed.target
        {
            return Err(failure("Reviewed credential target changed"));
        }
        Ok(())
    }

    pub(super) async fn check_written_credential_review(
        &self,
        reviewed: &CredentialReview,
        stored: &StoredSource,
    ) -> Result<StoredSource, kube::Error> {
        let current = self
            .review_credentials(
                &reviewed.target.namespace,
                &reviewed.target.kind,
                &reviewed.target.name,
                &reviewed.key,
            )
            .await?;
        if !reviewed.same_authority(&current) {
            return Err(failure(
                "Reviewed target or acknowledged write changed before binding",
            ));
        }
        // The target RV is still enforced by check_reviewed_target immediately
        // before binding. First retain any attested ownership-only source change.
        if reviewed.permits_refresh(&current, stored) {
            return Ok(stored.clone());
        }
        let grant = self.credential_grant(&reviewed.target.namespace).await?;
        let mut receipt_review = current.clone();
        receipt_review.grant = grant_review(&grant)?;
        let attested = grant.sources.iter().any(|source| {
            source.name == stored.name
                && source.uid == stored.uid
                && source.phase == "Ready"
                && source.ownership_from_resource_version.as_deref()
                    == Some(stored.version.as_str())
                && current.source.version.as_deref() == Some(source.resource_version.as_str())
                && source.target.as_ref().is_some_and(|target| {
                    target.kind == reviewed.target.kind
                        && target.namespace == reviewed.target.namespace
                        && target.name == reviewed.target.name
                        && Some(target.uid.as_str()) == reviewed.target.uid.as_deref()
                })
        });
        if !attested || !reviewed.same_authority(&receipt_review) {
            return Err(failure(
                "Source version changed without a matching controller ownership receipt",
            ));
        }
        let metadata = Api::<Secret>::namespaced(self.client.clone(), &reviewed.target.namespace)
            .get_metadata(&stored.name)
            .await
            .map_err(|error| safe("Verify controller ownership transition", error))?;
        let updated = StoredSource::from_metadata(&metadata.metadata)?;
        if metadata.metadata.namespace.as_deref() != Some(reviewed.target.namespace.as_str())
            || metadata.metadata.deletion_timestamp.is_some()
            || updated.uid != stored.uid
            || !reviewed.permits_refresh(&current, &updated)
        {
            return Err(failure(
                "Source changed after controller ownership acknowledgement",
            ));
        }
        Ok(updated)
    }

    pub(super) fn check_bound_credential_target(
        &self,
        before: &DynamicObject,
        patch: &Value,
        after: &DynamicObject,
    ) -> Result<(), kube::Error> {
        let mut expected = before.data["spec"].clone();
        json_patch::merge(&mut expected, patch);
        let mut old_metadata = before.metadata.clone();
        let mut new_metadata = after.metadata.clone();
        for metadata in [&mut old_metadata, &mut new_metadata] {
            metadata.resource_version = None;
            metadata.generation = None;
            metadata.managed_fields = None;
        }
        if expected != after.data["spec"]
            || old_metadata != new_metadata
            || before
                .metadata
                .generation
                .and_then(|generation| generation.checked_add(1))
                != after.metadata.generation
            || after.metadata.resource_version == before.metadata.resource_version
        {
            return Err(failure(
                "Credential binding response changed the approved intent",
            ));
        }
        Ok(())
    }

    pub(super) async fn check_completed_credential_review(
        &self,
        review: &CredentialReview,
        stored: &StoredSource,
        bound: Option<&DynamicObject>,
    ) -> Result<(), kube::Error> {
        let current = self
            .review_credentials(
                &review.target.namespace,
                &review.target.kind,
                &review.target.name,
                &review.key,
            )
            .await?;
        let expected_target = target_review(
            &review.target.kind,
            &review.target.namespace,
            &review.target.name,
            bound,
        )?;
        let mut target = current.target.clone();
        target.version.clone_from(&expected_target.version);
        let mut grant = current.grant.clone();
        grant.version.clone_from(&review.grant.version);
        let mut keys = review.source.keys.clone();
        keys.push(review.key.clone());
        keys.sort();
        keys.dedup();
        if target != expected_target
            || grant != review.grant
            || !stored.matches(&current.source)
            || current.source.keys != keys
        {
            return Err(failure(
                "Credential write changed before its completion could be verified",
            ));
        }
        Ok(())
    }

    pub async fn review_stored_credentials(
        &self,
        original: &CredentialReview,
        stored: &StoredSource,
    ) -> Result<CredentialReview, kube::Error> {
        if stored.name != original.source.name {
            return Err(failure("Continuation source name changed"));
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.await_source_metadata(
                &original.target.namespace,
                &Identity {
                    name: stored.name.clone(),
                    uid: stored.uid.clone(),
                },
            ),
        )
        .await
        .map_err(|_| failure("Source acknowledgement review deadline"))??;
        let current = self
            .review_credentials(
                &original.target.namespace,
                &original.target.kind,
                &original.target.name,
                &original.key,
            )
            .await?;
        if !original.permits_refresh(&current, stored) {
            return Err(failure(
                "Continuation authority, intent or acknowledged source changed",
            ));
        }
        Ok(current)
    }

    pub async fn review_unwritten_credentials(
        &self,
        original: &CredentialReview,
    ) -> Result<CredentialReview, kube::Error> {
        let current = self
            .review_credentials(
                &original.target.namespace,
                &original.target.kind,
                &original.target.name,
                &original.key,
            )
            .await?;
        if !original.permits_unwritten_refresh(&current) {
            return Err(failure(
                "Unwritten credential review authority or source changed",
            ));
        }
        Ok(current)
    }
}
