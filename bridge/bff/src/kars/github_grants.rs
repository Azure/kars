use super::{
    cluster::Cluster,
    credential_contract::{GitHubBinding, Identity},
    credentials::failure,
};
use k8s_openapi::api::core::v1::ConfigMap;
use kube::{Api, ResourceExt};
use sha2::{Digest, Sha256};

impl Cluster {
    pub async fn github_connection_grant(
        &self,
        namespace: &str,
        subject: &str,
        repositories: Vec<String>,
        write: bool,
    ) -> Result<GitHubBinding, kube::Error> {
        let grant = self.credential_grant(namespace).await?;
        let name = format!(
            "kars-github-connection-{}",
            hex::encode(&Sha256::digest(subject.as_bytes())[..8])
        );
        let connection = Api::<ConfigMap>::namespaced(self.client.clone(), namespace)
            .get(&name)
            .await?;
        let uid = connection
            .uid()
            .ok_or_else(|| failure("GitHub connection UID is missing"))?;
        let approved=grant.document.data["spec"]["githubConnections"].as_array()
            .and_then(|entries|entries.iter().find(|entry|
                entry["ownerSubject"]==subject && entry["connection"]["name"]==name && entry["connection"]["uid"]==uid))
            .ok_or_else(||failure("GitHub connection requires explicit operator App/installation/repository enrollment; no legacy token fallback"))?;
        let repositories = repositories
            .into_iter()
            .map(|repo| repo.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let allowed = approved["repositories"]
            .as_array()
            .ok_or_else(|| failure("GitHub repository grant is malformed"))?;
        let installation = connection
            .data
            .as_ref()
            .and_then(|data| data.get("installation_id"))
            .and_then(|id| id.parse::<u64>().ok());
        if connection.metadata.deletion_timestamp.is_some()
            || installation != approved["installationId"].as_u64()
            || repositories.is_empty()
            || repositories.len() > 32
            || repositories.iter().any(|repo| {
                !allowed
                    .iter()
                    .any(|value| value.as_str() == Some(repo.as_str()))
            })
            || (write && approved["write"] != true)
        {
            return Err(failure(
                "GitHub connection incarnation, installation, repositories or write authority changed",
            ));
        }
        Ok(GitHubBinding {
            grant: grant.identity,
            connection: Identity { name, uid },
            repositories,
            write,
        })
    }
}
