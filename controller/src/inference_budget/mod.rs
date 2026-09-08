// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

pub mod account;
pub mod admission;
pub mod auth;
pub mod binding;
pub mod claim;
pub mod config;
pub mod pod;
pub mod recovery;
pub mod scope;
pub mod service;
pub mod store;
pub mod team;
pub mod transport;

pub fn start(client: kube::Client) {
    match config::Settings::from_env() {
        Ok(None) => {}
        Err(_) => tracing::error!(
            "Governed inference broker configuration is invalid; finite inference remains unavailable"
        ),
        Ok(Some(settings)) => {
            recovery::start(client.clone(), settings.clone());
            tokio::spawn(async move {
                loop {
                    if transport::run(client.clone(), settings.clone())
                        .await
                        .is_err()
                    {
                        tracing::error!(
                            "Governed inference broker unavailable; finite inference remains fail-closed"
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
            });
        }
    }
}
