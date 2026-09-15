// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use reqwest::{Client, ClientBuilder, Method, RequestBuilder};

#[derive(Clone, Copy)]
pub(super) enum CopilotEndpoint {
    DeviceCode,
    AccessToken,
    Seat,
    Models,
}

impl CopilotEndpoint {
    fn url(self) -> &'static str {
        match self {
            Self::DeviceCode => "https://github.com/login/device/code",
            Self::AccessToken => "https://github.com/login/oauth/access_token",
            Self::Seat => "https://api.github.com/copilot_internal/v2/token",
            Self::Models => "https://api.githubcopilot.com/models",
        }
    }

    fn method(self) -> Method {
        match self {
            Self::DeviceCode | Self::AccessToken => Method::POST,
            Self::Seat | Self::Models => Method::GET,
        }
    }
}

/// The client and destinations are owned together: callers cannot substitute
/// a redirect-following client or an operator/upstream-supplied credential URL.
pub(super) struct CopilotClient {
    client: Client,
    #[cfg(test)]
    loopback: Option<std::net::SocketAddr>,
}

impl CopilotClient {
    fn builder() -> ClientBuilder {
        Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
    }

    pub(super) fn github() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Self::builder().build()?,
            #[cfg(test)]
            loopback: None,
        })
    }

    pub(super) fn request(&self, endpoint: CopilotEndpoint) -> RequestBuilder {
        #[cfg(test)]
        if let Some(address) = self.loopback {
            let path = reqwest::Url::parse(endpoint.url())
                .expect("static GitHub URL")
                .path()
                .to_owned();
            return self
                .client
                .request(endpoint.method(), format!("http://{address}{path}"));
        }
        self.client.request(endpoint.method(), endpoint.url())
    }

    // Only unit-test builds can send synthetic credentials over HTTP. Accept a
    // literal loopback address, never a hostname, environment variable or URL.
    #[cfg(test)]
    pub(super) fn loopback(address: std::net::SocketAddr) -> Self {
        assert!(address.ip().is_loopback() && address.port() != 0);
        Self {
            client: Self::builder()
                .https_only(false)
                .no_proxy()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .unwrap(),
            loopback: Some(address),
        }
    }
}

#[cfg(test)]
#[path = "copilot_transport_tests.rs"]
mod tests;
