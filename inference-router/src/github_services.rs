// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Optional atomic Secret projection. Changes discard all installation caches;
//! removal/invalid replacement never falls back to the previous credential.

use crate::{
    access_request::Identity,
    github_app::{Error, GitHubApp},
};
use serde::Deserialize;
#[cfg(test)]
use std::path::PathBuf;
use std::{io::Read, sync::Arc};
use tokio::sync::Mutex;

pub(crate) const CONFIG_PATH: &str = "/etc/kars/github/config.json";
const MAX_CONFIG: usize = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    identity: Identity,
    app_id: String,
    installation_id: u64,
    private_key_pem: String,
    repositories: Vec<String>,
    #[serde(default)]
    write: bool,
}

struct Loaded {
    source: Vec<u8>,
    app: Arc<GitHubApp>,
}

pub(crate) struct GitHubServices {
    #[cfg(test)]
    path: PathBuf,
    identity: Option<Identity>,
    client: reqwest::Client,
    loaded: Mutex<Option<Loaded>>,
}

impl GitHubServices {
    pub(crate) fn new(identity: Option<Identity>, client: reqwest::Client) -> Self {
        Self {
            #[cfg(test)]
            path: CONFIG_PATH.into(),
            identity,
            client,
            loaded: Mutex::new(None),
        }
    }

    pub(crate) async fn current(&self) -> Result<Option<Arc<GitHubApp>>, Error> {
        let mut loaded = self.loaded.lock().await;
        let bytes = match self.read() {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                *loaded = None;
                return Ok(None);
            }
            Err(error) => {
                *loaded = None;
                return Err(error);
            }
        };
        if let Some(current) = loaded.as_ref()
            && current.source == bytes
        {
            return Ok(Some(current.app.clone()));
        }
        *loaded = None;
        let config: Config = serde_json::from_slice(&bytes).map_err(|_| Error::Configuration)?;
        if self.identity.as_ref() != Some(&config.identity)
            || !config.identity.managed
            || !config.identity.valid(&config.identity.sandbox.name)
        {
            return Err(Error::Configuration);
        }
        let repositories = config
            .repositories
            .iter()
            .map(|repo| crate::github_app::repository(repo).ok_or(Error::Configuration))
            .collect::<Result<Vec<_>, _>>()?;
        let app = GitHubApp::new(
            config.app_id,
            config.installation_id,
            config.private_key_pem.as_bytes(),
            repositories,
            config.write,
            self.client.clone(),
        )?;
        *loaded = Some(Loaded {
            source: bytes,
            app: app.clone(),
        });
        Ok(Some(app))
    }

    #[cfg(not(test))]
    fn read(&self) -> Result<Option<Vec<u8>>, Error> {
        Self::read_file(std::fs::File::open(CONFIG_PATH))
    }

    fn read_file(file: std::io::Result<std::fs::File>) -> Result<Option<Vec<u8>>, Error> {
        let file = match file {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::Configuration),
        };
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Configuration)?;
        if bytes.len() > MAX_CONFIG {
            return Err(Error::Configuration);
        }
        Ok(Some(bytes))
    }

    pub(crate) async fn unchanged(&self, app: &Arc<GitHubApp>) -> bool {
        matches!(self.current().await, Ok(Some(current)) if Arc::ptr_eq(&current, app))
    }
}

#[cfg(test)]
#[path = "github_services_tests.rs"]
pub(crate) mod tests;
